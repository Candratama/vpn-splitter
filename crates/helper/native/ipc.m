#import <Foundation/Foundation.h>
#import <Security/Security.h>
#import <SystemConfiguration/SystemConfiguration.h>
#import <ServiceManagement/ServiceManagement.h>
#include <stdatomic.h>
#include <errno.h>
#include <unistd.h>
#import <IOKit/pwr_mgt/IOPMLib.h>
#import <IOKit/IOMessage.h>
#include "native.h"
#define VS_LIMIT 1048576
#define VS_SERVICE @"com.candratama.vpnsplitter.helper"
#define VS_APP @"com.candratama.vpnsplitter"

typedef int (*VSRequest)(void *,uint32_t,uint64_t,const char *,const uint8_t *,size_t,uint8_t *,size_t,size_t *);
typedef void (*VSInvalidated)(void *,uint64_t);
@protocol VSProtocol
- (void)exchange:(NSData *)data reply:(void (^)(NSData *,int))reply;
@end
static NSString *self_requirement(NSString *identifier) {
    SecCodeRef code=NULL;if(SecCodeCopySelf(kSecCSDefaultFlags,&code)!=errSecSuccess)return nil;
    CFDictionaryRef raw=NULL;OSStatus result=SecCodeCopySigningInformation(code,kSecCSSigningInformation,&raw);CFRelease(code);
    if(result!=errSecSuccess)return nil;
    NSDictionary *info=CFBridgingRelease(raw);NSString *team=info[(__bridge NSString*)kSecCodeInfoTeamIdentifier];
    if(!team || team.length!=10 || [team rangeOfCharacterFromSet:[[NSCharacterSet alphanumericCharacterSet] invertedSet]].location!=NSNotFound)return nil;
    return [NSString stringWithFormat:@"anchor apple generic and identifier \"%@\" and certificate leaf[subject.OU] = \"%@\"",identifier,team];
}
uint32_t vs_console_uid(void) {
    uid_t uid=0;CFStringRef user=SCDynamicStoreCopyConsoleUser(NULL,&uid,NULL);if(user)CFRelease(user);return uid;
}
int vs_code_requirement(uint8_t *out,size_t capacity,size_t *length) { @autoreleasepool {
    NSString *requirement=self_requirement(VS_APP);if(!requirement)return EACCES;
    NSData *data=[requirement dataUsingEncoding:NSUTF8StringEncoding];if(data.length>capacity)return EOVERFLOW;
    memcpy(out,data.bytes,data.length);*length=data.length;return 0;
}}
@interface VSExport:NSObject<VSProtocol>
@property VSRequest callback;
@property VSInvalidated invalidated;
@property void *context;
@property uint64_t identifier;
@property uint32_t uid;
@property(copy) NSString *requirement;
@property dispatch_queue_t queue;
@end
@implementation VSExport
- (void)exchange:(NSData *)data reply:(void (^)(NSData *,int))reply {
    NSXPCConnection *connection=[NSXPCConnection currentConnection];
    if(connection && (connection.effectiveUserIdentifier!=self.uid || self.uid!=vs_console_uid())){reply(nil,EACCES);return;}
    if(!data || data.length>VS_LIMIT){reply(nil,EMSGSIZE);return;}
    dispatch_async(self.queue,^{
        uint8_t *out=malloc(VS_LIMIT);if(!out){reply(nil,ENOMEM);return;}
        size_t length=0;int result=self.callback(self.context,self.uid,self.identifier,self.requirement.UTF8String,data.bytes,data.length,out,VS_LIMIT,&length);
        NSData *response=result==0 && length<=VS_LIMIT?[NSData dataWithBytes:out length:length]:nil;free(out);reply(response,response?0:(result?result:EIO));
    });
}
@end
@interface VSListener:NSObject<NSXPCListenerDelegate>
@property VSRequest callback;
@property VSInvalidated invalidated;
@property void *context;
@property(copy) NSString *requirement;
@property dispatch_queue_t queue;
@property NSMutableSet<NSXPCConnection *> *connections;
@end
@implementation VSListener
- (BOOL)listener:(NSXPCListener *)listener shouldAcceptNewConnection:(NSXPCConnection *)connection {
    (void)listener;
    uint32_t uid=connection.effectiveUserIdentifier;
    if(uid==0 || uid!=vs_console_uid())return NO;
    [connection setCodeSigningRequirement:self.requirement];
    static _Atomic uint64_t next_identifier=1;
    uint64_t identifier=atomic_fetch_add(&next_identifier,1);
    VSExport *object=[VSExport new];object.callback=self.callback;object.invalidated=self.invalidated;object.context=self.context;object.identifier=identifier;object.uid=uid;object.requirement=self.requirement;object.queue=self.queue;
    connection.exportedInterface=[NSXPCInterface interfaceWithProtocol:@protocol(VSProtocol)];connection.exportedObject=object;
    __weak NSXPCConnection *weak=connection;
    connection.invalidationHandler=^{dispatch_async(self.queue,^{self.invalidated(self.context,identifier);NSXPCConnection *strong=weak;if(strong)@synchronized(self.connections){[self.connections removeObject:strong];}});};
    connection.interruptionHandler=^{[weak invalidate];};
    @synchronized(self.connections){[self.connections addObject:connection];}
    [connection activate];return YES;
}
@end
static SCDynamicStoreRef monitoring_store=NULL;
static dispatch_queue_t monitoring_queue;
static BOOL monitoring_active=NO;
static IONotificationPortRef power_port=NULL;
static io_connect_t power_root=0;
static io_object_t power_notifier=0;
static CFRunLoopRef service_run_loop=NULL;
static VSListener *service_listener;
static void network_changed(SCDynamicStoreRef,CFArrayRef,void *);
static void power_event(void *context,io_service_t service,natural_t message,void *argument) {
    (void)service;
    if(message==kIOMessageSystemWillSleep || message==kIOMessageCanSystemSleep)IOAllowPowerChange(power_root,(long)argument);
    if(message==kIOMessageSystemHasPoweredOn)network_changed(NULL,NULL,context);
}

int vs_monitor(int active) {
    if(!monitoring_store)return EIO;
    if(monitoring_active==(active!=0))return 0;
    if(!active){
        SCDynamicStoreSetDispatchQueue(monitoring_store,NULL);SCDynamicStoreSetNotificationKeys(monitoring_store,NULL,NULL);
        if(power_root){CFRunLoopRemoveSource(service_run_loop,IONotificationPortGetRunLoopSource(power_port),kCFRunLoopDefaultMode);IODeregisterForSystemPower(&power_notifier);IOServiceClose(power_root);IONotificationPortDestroy(power_port);power_root=0;power_port=NULL;}
        monitoring_active=NO;return 0;
    }
    NSArray *patterns=@[@"State:/Network/Global/IPv4",@"State:/Network/Service/.*/IPv4",@"State:/Network/Service/.*/PPP",@"State:/Users/ConsoleUser"];
    if(!SCDynamicStoreSetNotificationKeys(monitoring_store,NULL,(__bridge CFArrayRef)patterns) || !SCDynamicStoreSetDispatchQueue(monitoring_store,monitoring_queue))return EIO;
    power_root=IORegisterForSystemPower((__bridge void*)service_listener,&power_port,power_event,&power_notifier);
    if(!power_root){SCDynamicStoreSetDispatchQueue(monitoring_store,NULL);return EIO;}
    CFRunLoopAddSource(service_run_loop,IONotificationPortGetRunLoopSource(power_port),kCFRunLoopDefaultMode);
    monitoring_active=YES;return 0;
}
static void network_changed(SCDynamicStoreRef store,CFArrayRef keys,void *context) {
    (void)store;(void)keys;VSListener *listener=(__bridge VSListener*)context;
    @synchronized(listener.connections){for(NSXPCConnection *connection in listener.connections.copy)[connection invalidate];}
}
int vs_serve(void *context,VSRequest request,VSInvalidated invalidated) { @autoreleasepool {
    if(geteuid()!=0)return EACCES;
    NSString *requirement=self_requirement(VS_APP);if(!requirement)return EACCES;
    VSListener *delegate=[VSListener new];delegate.context=context;delegate.callback=request;delegate.invalidated=invalidated;delegate.requirement=requirement;delegate.queue=dispatch_queue_create("com.candratama.vpnsplitter.routes",DISPATCH_QUEUE_SERIAL);delegate.connections=[NSMutableSet set];
    NSXPCListener *listener=[[NSXPCListener alloc]initWithMachServiceName:VS_SERVICE];[listener setConnectionCodeSigningRequirement:requirement];listener.delegate=delegate;
    SCDynamicStoreContext observer_context={0,(__bridge void*)delegate,NULL,NULL,NULL};
    SCDynamicStoreRef store=SCDynamicStoreCreate(NULL,CFSTR("VPN Splitter helper"),network_changed,&observer_context);
    if(!store)return EIO;
    monitoring_store=store;monitoring_queue=delegate.queue;service_run_loop=CFRunLoopGetCurrent();service_listener=delegate;
    [listener activate];[[NSRunLoop currentRunLoop]run];
    [listener invalidate];SCDynamicStoreSetDispatchQueue(store,NULL);monitoring_store=NULL;CFRelease(store);return 0;
}}
void *vs_client_create(void) { @autoreleasepool {
    NSString *requirement=self_requirement(VS_SERVICE);if(!requirement)return NULL;
    NSXPCConnection *connection=[[NSXPCConnection alloc]initWithMachServiceName:VS_SERVICE options:NSXPCConnectionPrivileged];
    connection.remoteObjectInterface=[NSXPCInterface interfaceWithProtocol:@protocol(VSProtocol)];[connection setCodeSigningRequirement:requirement];[connection activate];
    return (__bridge_retained void*)connection;
}}
int vs_client_request(void *client,const uint8_t *bytes,size_t length,uint8_t *out,size_t capacity,size_t *out_length) { @autoreleasepool {
    if(!client || length>VS_LIMIT)return EINVAL;
    NSXPCConnection *connection=(__bridge NSXPCConnection*)client;
    dispatch_semaphore_t semaphore=dispatch_semaphore_create(0);NSObject *lock=[NSObject new];
    __block NSData *response=nil;__block int error=ETIMEDOUT;
    id<VSProtocol> proxy=[connection remoteObjectProxyWithErrorHandler:^(NSError *e){(void)e;@synchronized(lock){error=EIO;}dispatch_semaphore_signal(semaphore);}];
    [proxy exchange:[NSData dataWithBytes:bytes length:length] reply:^(NSData *data,int code){@synchronized(lock){response=data;error=code;}dispatch_semaphore_signal(semaphore);}];
    if(dispatch_semaphore_wait(semaphore,dispatch_time(DISPATCH_TIME_NOW,10*NSEC_PER_SEC))!=0){[connection invalidate];return ETIMEDOUT;}
    @synchronized(lock){if(error)return error;if(!response || response.length>capacity)return EMSGSIZE;memcpy(out,response.bytes,response.length);*out_length=response.length;}
    return 0;
}}
void vs_client_close(void *client) {if(client){NSXPCConnection *connection=CFBridgingRelease(client);[connection invalidate];}}
int vs_register_helper(int unregistering) { @autoreleasepool {
    if(!self_requirement(VS_SERVICE))return EACCES;
    SMAppService *service=[SMAppService daemonServiceWithPlistName:@"com.candratama.vpnsplitter.helper.plist"];
    NSError *error=nil;BOOL success=unregistering?[service unregisterAndReturnError:&error]:[service registerAndReturnError:&error];
    if(!success)return EACCES;
    return service.status==SMAppServiceStatusEnabled || unregistering?0:EAGAIN;
}}

// Unprivileged anonymous-listener probe; no helper registration or route calls.
@interface VSProbe:NSObject<VSProtocol>
@property NSUInteger calls;
@end
@implementation VSProbe
- (void)exchange:(NSData *)data reply:(void (^)(NSData *,int))reply {self.calls++;reply(data,0);}
@end
@interface VSProbeDelegate:NSObject<NSXPCListenerDelegate>
@property VSProbe *probe;
@property NSString *requirement;
@end
@implementation VSProbeDelegate
- (BOOL)listener:(NSXPCListener *)listener shouldAcceptNewConnection:(NSXPCConnection *)connection {
    (void)listener;
    if(connection.effectiveUserIdentifier!=getuid())return NO;
    [connection setCodeSigningRequirement:self.requirement];
    connection.exportedInterface=[NSXPCInterface interfaceWithProtocol:@protocol(VSProtocol)];connection.exportedObject=self.probe;[connection activate];return YES;
}
@end
int vs_xpc_probe(void) { @autoreleasepool {
    SecCodeRef code=NULL;SecRequirementRef designated=NULL;CFStringRef raw=NULL;
    if(SecCodeCopySelf(kSecCSDefaultFlags,&code)!=errSecSuccess)return 1;
    OSStatus status=SecCodeCopyDesignatedRequirement(code,kSecCSDefaultFlags,&designated);CFRelease(code);if(status!=errSecSuccess)return 2;
    status=SecRequirementCopyString(designated,kSecCSDefaultFlags,&raw);CFRelease(designated);if(status!=errSecSuccess)return 3;
    NSString *own_requirement=CFBridgingRelease(raw);
    for(int valid=0;valid<2;valid++) {
        NSString *requirement=valid?own_requirement:@"cdhash H\"0000000000000000000000000000000000000000\"";
        VSProbeDelegate *delegate=[VSProbeDelegate new];delegate.probe=[VSProbe new];delegate.requirement=requirement;
        NSXPCListener *listener=[NSXPCListener anonymousListener];[listener setConnectionCodeSigningRequirement:requirement];listener.delegate=delegate;[listener activate];
        NSXPCConnection *connection=[[NSXPCConnection alloc]initWithListenerEndpoint:listener.endpoint];connection.remoteObjectInterface=[NSXPCInterface interfaceWithProtocol:@protocol(VSProtocol)];[connection activate];
        dispatch_semaphore_t semaphore=dispatch_semaphore_create(0);__block int error=ETIMEDOUT;
        id<VSProtocol> proxy=[connection remoteObjectProxyWithErrorHandler:^(NSError *e){(void)e;error=EACCES;dispatch_semaphore_signal(semaphore);}];
        [proxy exchange:[NSData dataWithBytes:"{}" length:2] reply:^(NSData *data,int code){error=(data.length==2)?code:EIO;dispatch_semaphore_signal(semaphore);}];
        long timeout=dispatch_semaphore_wait(semaphore,dispatch_time(DISPATCH_TIME_NOW,3*NSEC_PER_SEC));
        [connection invalidate];[listener invalidate];
        if(timeout!=0)return 4+valid;
        if(valid && (error!=0 || delegate.probe.calls!=1))return 6;
        if(!valid && (error==0 || delegate.probe.calls!=0))return 7;
    }
    return 0;
}}
int vs_helper_status(void) { @autoreleasepool {
    if(!self_requirement(VS_SERVICE))return -1;
    return (int)[SMAppService daemonServiceWithPlistName:@"com.candratama.vpnsplitter.helper.plist"].status;
}}
