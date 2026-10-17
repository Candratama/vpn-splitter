#import <Foundation/Foundation.h>
#import <SystemConfiguration/SystemConfiguration.h>
#include <arpa/inet.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <net/if_dl.h>
#include <net/route.h>
#include <sys/socket.h>
#include <sys/sysctl.h>
#include <unistd.h>
#include <errno.h>
#include <poll.h>
#include "native.h"

static size_t rounded(size_t n) { return n ? (n + sizeof(uint32_t)-1) & ~(sizeof(uint32_t)-1) : sizeof(uint32_t); }
static int decode(const uint8_t *bytes, size_t length, VSRoute *out, size_t capacity, size_t *count) {
    *count=0;
    size_t offset=0;
    while(offset<length) {
        if(length-offset<sizeof(struct rt_msghdr)) return EINVAL;
        struct rt_msghdr h; memcpy(&h,bytes+offset,sizeof(h));
        if(h.rtm_msglen<sizeof(h) || h.rtm_msglen>length-offset || h.rtm_version!=RTM_VERSION) return EINVAL;
        size_t cursor=offset+sizeof(h),end=offset+h.rtm_msglen;
        const struct sockaddr *addresses[RTAX_MAX]={0};
        for(int i=0;i<RTAX_MAX;i++) if(h.rtm_addrs & (1<<i)) {
            if(end-cursor<2) return EINVAL;
            const struct sockaddr *sa=(const void *)(bytes+cursor);
            size_t size=rounded(sa->sa_len);
            if(size>end-cursor || (sa->sa_len!=0 && sa->sa_len<2)) return EINVAL;
            addresses[i]=sa;cursor+=size;
        }
        const struct sockaddr_in *dst=(const void *)addresses[RTAX_DST];
        if(dst && dst->sin_family==AF_INET && dst->sin_len>=sizeof(*dst) && (h.rtm_flags&RTF_UP) && !(h.rtm_flags&RTF_IFSCOPE)) {
            uint32_t mask=0;
            if(h.rtm_flags&RTF_HOST) mask=UINT32_MAX;
            else if(addresses[RTAX_NETMASK]) {
                const struct sockaddr *sa=addresses[RTAX_NETMASK];
                if(sa->sa_len>offsetof(struct sockaddr_in,sin_addr)) {
                    size_t n=sa->sa_len - offsetof(struct sockaddr_in,sin_addr);
                    if(n>4) n=4;
                    memcpy(&mask,(const uint8_t*)sa+offsetof(struct sockaddr_in,sin_addr),n);mask=ntohl(mask);
                }
            }
            uint32_t inverse=~mask;
            if((inverse & (inverse+1))!=0) return EINVAL;
            VSRoute route={.destination=ntohl(dst->sin_addr.s_addr),.prefix=(uint8_t)__builtin_popcount(mask),.interface_id=h.rtm_index};
            const struct sockaddr_in *gw=(const void*)addresses[RTAX_GATEWAY];
            if(gw && gw->sin_family==AF_INET && gw->sin_len>=sizeof(*gw) && (h.rtm_flags&RTF_GATEWAY)) { route.has_gateway=1;route.gateway=ntohl(gw->sin_addr.s_addr); }
            if(*count==capacity) return EOVERFLOW;
            out[(*count)++]=route;
        }
        offset=end;
    }
    return 0;
}
int vs_routes(VSRoute *out,size_t capacity,size_t *count) {
    int mib[]={CTL_NET,PF_ROUTE,0,AF_INET,NET_RT_DUMP,0};
    for(int attempt=0;attempt<3;attempt++) {
        size_t size=0;if(sysctl(mib,6,NULL,&size,NULL,0)<0) return errno;
        if(size>16*1024*1024) return EOVERFLOW;
        uint8_t *bytes=malloc(size?size:1);if(!bytes) return ENOMEM;
        if(sysctl(mib,6,bytes,&size,NULL,0)<0) { int e=errno;free(bytes);if(e==ENOMEM)continue;return e; }
        int e=decode(bytes,size,out,capacity,count);free(bytes);return e;
    }
    return EAGAIN;
}
int vs_route_change(const VSRoute *route,int adding) {
    if(geteuid()!=0) return EPERM;
    if(route->prefix<2 || route->prefix>32 || route->interface_id==0) return EINVAL;
    struct { struct rt_msghdr h; uint8_t addresses[256]; } message={0};
    message.h.rtm_version=RTM_VERSION;message.h.rtm_type=adding?RTM_ADD:RTM_DELETE;
    message.h.rtm_flags=RTF_UP|RTF_STATIC|(route->prefix==32?RTF_HOST:0)|(route->has_gateway?RTF_GATEWAY:0);
    message.h.rtm_addrs=RTA_DST|RTA_GATEWAY|RTA_NETMASK|RTA_IFP;
    message.h.rtm_index=(unsigned short)route->interface_id;
    message.h.rtm_pid=getpid();message.h.rtm_seq=1;
    struct sockaddr_in dst={.sin_len=sizeof(dst),.sin_family=AF_INET,.sin_addr.s_addr=htonl(route->destination)};
    struct sockaddr_in gateway={.sin_len=sizeof(gateway),.sin_family=AF_INET,.sin_addr.s_addr=htonl(route->gateway)};
    struct sockaddr_in mask={.sin_len=sizeof(mask),.sin_family=AF_INET,.sin_addr.s_addr=htonl(UINT32_MAX<<(32-route->prefix))};
    struct sockaddr_dl link={.sdl_len=sizeof(link),.sdl_family=AF_LINK,.sdl_index=(unsigned short)route->interface_id};
    size_t used=0;
    const struct sockaddr *sa[]={ (void*)&dst,route->has_gateway?(void*)&gateway:(void*)&link,(void*)&mask,(void*)&link };
    for(int i=0;i<4;i++) { size_t size=rounded(sa[i]->sa_len);memcpy(message.addresses+used,sa[i],sa[i]->sa_len);used+=size; }
    message.h.rtm_msglen=(unsigned short)(sizeof(message.h)+used);
    int fd=socket(PF_ROUTE,SOCK_RAW,0);if(fd<0)return errno;
    if(write(fd,&message,message.h.rtm_msglen)!=(ssize_t)message.h.rtm_msglen) {int e=errno?errno:EIO;close(fd);return e;}
    int result=ETIMEDOUT;
    struct timespec start;clock_gettime(CLOCK_MONOTONIC,&start);
    while(1) {
        struct timespec now;clock_gettime(CLOCK_MONOTONIC,&now);
        long elapsed=(now.tv_sec-start.tv_sec)*1000+(now.tv_nsec-start.tv_nsec)/1000000;
        if(elapsed>=5000)break;
        struct pollfd poller={.fd=fd,.events=POLLIN};int ready=poll(&poller,1,(int)(5000-elapsed));
        if(ready<0){if(errno==EINTR)continue;result=errno;break;}if(ready==0)break;
        uint8_t bytes[8192];ssize_t n=read(fd,bytes,sizeof(bytes));if(n<(ssize_t)sizeof(struct rt_msghdr))continue;
        struct rt_msghdr h;memcpy(&h,bytes,sizeof(h));
        if(h.rtm_pid==getpid() && h.rtm_seq==1){result=h.rtm_errno;break;}
    }
    close(fd);return result;
}
int vs_route_parser_probe(void) {
    struct {struct rt_msghdr h;struct sockaddr_in dst;struct sockaddr_in gateway;struct sockaddr_in mask;} packet={0};
    packet.h.rtm_msglen=sizeof(packet);packet.h.rtm_version=RTM_VERSION;packet.h.rtm_flags=RTF_UP|RTF_GATEWAY;packet.h.rtm_addrs=RTA_DST|RTA_GATEWAY|RTA_NETMASK;packet.h.rtm_index=9;
    packet.dst=(struct sockaddr_in){.sin_len=sizeof(packet.dst),.sin_family=AF_INET,.sin_addr.s_addr=htonl(0xc6336400)};
    packet.gateway=(struct sockaddr_in){.sin_len=sizeof(packet.gateway),.sin_family=AF_INET,.sin_addr.s_addr=htonl(0xcb007101)};
    packet.mask=(struct sockaddr_in){.sin_len=sizeof(packet.mask),.sin_family=AF_INET,.sin_addr.s_addr=htonl(0xffffff00)};
    VSRoute route;size_t count=0;
    if(decode((void*)&packet,sizeof(packet),&route,1,&count)!=0 || count!=1 || route.prefix!=24 || route.interface_id!=9 || !route.has_gateway || route.gateway!=0xcb007101 || route.destination!=0xc6336400)return 1;
    for(size_t n=1;n<sizeof(packet);n++)if(decode((void*)&packet,n,&route,1,&count)==0)return 2;
    packet.mask.sin_addr.s_addr=htonl(0xff00ff00);if(decode((void*)&packet,sizeof(packet),&route,1,&count)==0)return 3;
    return 0;
}
static int output_json(id object,uint8_t *out,size_t capacity,size_t *length) {
    if(!object)return EINVAL;
    NSData *data=[NSJSONSerialization dataWithJSONObject:object options:0 error:nil];
    if(!data || data.length>capacity)return EOVERFLOW;
    memcpy(out,data.bytes,data.length);*length=data.length;return 0;
}
static SCNetworkServiceRef service_with_id(NSString *identifier,SCPreferencesRef prefs) {
    return SCNetworkServiceCopy(prefs,(__bridge CFStringRef)identifier);
}
static BOOL supported(SCNetworkServiceRef service) {
    SCNetworkInterfaceRef interface=SCNetworkServiceGetInterface(service);
    return interface && CFEqual(SCNetworkInterfaceGetInterfaceType(interface),kSCNetworkInterfaceTypePPP) && SCNetworkInterfaceGetInterface(interface)
        && CFEqual(SCNetworkInterfaceGetInterfaceType(SCNetworkInterfaceGetInterface(interface)),kSCNetworkInterfaceTypeL2TP);
}
static BOOL split(SCNetworkServiceRef service) {
    SCNetworkProtocolRef protocol=SCNetworkServiceCopyProtocol(service,kSCNetworkProtocolTypeIPv4);
    if(!protocol)return NO;
    NSDictionary *config=(__bridge NSDictionary*)SCNetworkProtocolGetConfiguration(protocol);
    BOOL value=config && ![config[@"OverridePrimary"] boolValue];CFRelease(protocol);return value;
}
int vs_profiles(uint8_t *out,size_t capacity,size_t *length) { @autoreleasepool {
    SCPreferencesRef prefs=SCPreferencesCreate(NULL,CFSTR("VPN Splitter"),NULL);if(!prefs)return EIO;
    NSArray *services=CFBridgingRelease(SCNetworkServiceCopyAll(prefs));NSMutableArray *profiles=[NSMutableArray array];
    for(id value in services) {
        SCNetworkServiceRef service=(__bridge SCNetworkServiceRef)value;
        SCNetworkInterfaceRef interface=SCNetworkServiceGetInterface(service);
        if(!interface || !CFEqual(SCNetworkInterfaceGetInterfaceType(interface),kSCNetworkInterfaceTypePPP))continue;
        NSString *identifier=(__bridge NSString*)SCNetworkServiceGetServiceID(service);
        SCNetworkConnectionRef connection=SCNetworkConnectionCreateWithServiceID(NULL,(__bridge CFStringRef)identifier,NULL,NULL);
        BOOL connected=connection && SCNetworkConnectionGetStatus(connection)==kSCNetworkConnectionConnected;
        if(connection)CFRelease(connection);
        [profiles addObject:@{@"id":identifier,@"name":(__bridge NSString*)SCNetworkServiceGetName(service)?:@"VPN",@"supported":@(supported(service)),@"split_enabled":@(split(service)),@"connected":@(connected)}];
    }
    CFRelease(prefs);return output_json(profiles,out,capacity,length);
}}
static NSString *ip_string(uint32_t ip) {struct in_addr address={.s_addr=htonl(ip)};char text[INET_ADDRSTRLEN];if(!inet_ntop(AF_INET,&address,text,sizeof(text)))return nil;return @(text);}
static uint32_t parse_ip(NSString *text) {struct in_addr address;if(!text || inet_pton(AF_INET,text.UTF8String,&address)!=1)return 0;return ntohl(address.s_addr);}
static uint32_t interface_ip(uint32_t index) {
    struct ifaddrs *list=NULL;if(getifaddrs(&list)!=0)return 0;uint32_t result=0;
    for(struct ifaddrs *p=list;p;p=p->ifa_next)if(p->ifa_addr && p->ifa_addr->sa_family==AF_INET && if_nametoindex(p->ifa_name)==index){result=ntohl(((struct sockaddr_in*)p->ifa_addr)->sin_addr.s_addr);break;}
    freeifaddrs(list);return result;
}
static NSDictionary *route_json(VSRoute route) {return @{@"destination":ip_string(route.destination),@"prefix":@(route.prefix),@"interface_id":@(route.interface_id),@"gateway":route.has_gateway?ip_string(route.gateway):(id)[NSNull null]};}
int vs_snapshot(const char *profile,uint8_t *out,size_t capacity,size_t *length) { @autoreleasepool {
    NSString *identifier=@(profile);SCPreferencesRef prefs=SCPreferencesCreate(NULL,CFSTR("VPN Splitter"),NULL);if(!prefs)return EIO;
    SCNetworkServiceRef service=service_with_id(identifier,prefs);if(!service){CFRelease(prefs);return ENOENT;}
    if(!supported(service) || !split(service)){CFRelease(service);CFRelease(prefs);return ENOTSUP;}
    SCDynamicStoreRef store=SCDynamicStoreCreate(NULL,CFSTR("VPN Splitter"),NULL,NULL);
    NSDictionary *state=store?CFBridgingRelease(SCDynamicStoreCopyValue(store,(__bridge CFStringRef)[NSString stringWithFormat:@"State:/Network/Service/%@/IPv4",identifier])):nil;
    NSString *name=state[@"InterfaceName"];uint32_t vpn=name?if_nametoindex(name.UTF8String):0;
    SCNetworkConnectionRef connection=SCNetworkConnectionCreateWithServiceID(NULL,(__bridge CFStringRef)identifier,NULL,NULL);
    BOOL connected=connection && SCNetworkConnectionGetStatus(connection)==kSCNetworkConnectionConnected;if(connection)CFRelease(connection);
    NSDictionary *config=(__bridge NSDictionary*)SCNetworkInterfaceGetConfiguration(SCNetworkServiceGetInterface(service));
    uint32_t endpoint=parse_ip(config[@"CommRemoteAddress"]);
    if(!connected || !vpn || !endpoint){if(store)CFRelease(store);CFRelease(service);CFRelease(prefs);return ENOTSUP;}
    VSRoute *routes=calloc(65536,sizeof(VSRoute));size_t count=0;int error=routes?vs_routes(routes,65536,&count):ENOMEM;
    VSRoute physical={0},endpoint_route={0};BOOL found_endpoint=NO;
    if(!error)for(size_t i=0;i<count;i++) {
        VSRoute r=routes[i];if(r.prefix==0 && r.has_gateway)physical=r;
        uint32_t mask=r.prefix?UINT32_MAX<<(32-r.prefix):0;
        if((endpoint&mask)==(r.destination&mask) && (!found_endpoint || r.prefix>endpoint_route.prefix)){endpoint_route=r;found_endpoint=YES;}
    }
    NSMutableArray *existing=[NSMutableArray array];if(!error)for(size_t i=0;i<count;i++)[existing addObject:route_json(routes[i])];free(routes);
    NSDictionary *dns=store?CFBridgingRelease(SCDynamicStoreCopyValue(store,CFSTR("State:/Network/Global/DNS"))):nil;
    uint32_t vpn_ip=interface_ip(vpn),physical_ip=interface_ip(physical.interface_id);
    NSDictionary *snapshot=nil;
    if(!error && vpn_ip && physical_ip && found_endpoint) snapshot=@{@"profile_id":identifier,@"profile_exists":@YES,@"supported":@YES,@"split_enabled":@YES,@"vpn":@{@"index":@(vpn),@"address":ip_string(vpn_ip),@"gateway":[NSNull null]},@"physical":@{@"index":@(physical.interface_id),@"address":ip_string(physical_ip),@"gateway":ip_string(physical.gateway)},@"default_interface_id":@(physical.interface_id),@"vpn_endpoint":ip_string(endpoint),@"endpoint_interface_id":@(endpoint_route.interface_id),@"dns_usable":@([dns[@"ServerAddresses"] count]>0),@"existing_routes":existing};
    if(store)CFRelease(store);CFRelease(service);CFRelease(prefs);return error?error:output_json(snapshot,out,capacity,length);
}}
int vs_vpn_control(const char *profile,int connect) { @autoreleasepool {
    if(connect) {
        SCPreferencesRef prefs=SCPreferencesCreate(NULL,CFSTR("VPN Splitter"),NULL);if(!prefs)return EIO;
        SCNetworkServiceRef service=service_with_id(@(profile),prefs);
        BOOL valid=service && supported(service) && split(service);if(service)CFRelease(service);CFRelease(prefs);if(!valid)return ENOTSUP;
    }
    SCNetworkConnectionRef connection=SCNetworkConnectionCreateWithServiceID(NULL,(__bridge CFStringRef)@(profile),NULL,NULL);if(!connection)return ENOENT;
    Boolean success=connect?SCNetworkConnectionStart(connection,NULL,TRUE):SCNetworkConnectionStop(connection,TRUE);CFRelease(connection);return success?0:EACCES;
}}
