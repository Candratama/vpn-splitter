#include <stdint.h>
#include <stddef.h>
typedef struct { uint32_t destination, interface_id, gateway; uint8_t prefix, has_gateway; uint8_t padding[2]; } VSRoute;
int vs_routes(VSRoute *, size_t, size_t *);
int vs_route_change(const VSRoute *, int);
int vs_route_parser_probe(void);
int vs_profiles(uint8_t *, size_t, size_t *);
int vs_snapshot(const char *, uint8_t *, size_t, size_t *);
int vs_vpn_control(const char *, int);
