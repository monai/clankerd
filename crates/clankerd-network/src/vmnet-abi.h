/* Cross-build declarations for the public macOS 15 vmnet ABI.
 * Native builds compile against Apple's SDK, checking these uses there too. */
#include <sys/uio.h>
#include <xpc/xpc.h>
#include <dispatch/dispatch.h>
#include <stdint.h>
typedef void *interface_ref;
typedef uint32_t vmnet_return_t;
struct vmpktdesc { size_t vm_pkt_size; struct iovec *vm_pkt_iov; uint32_t vm_pkt_iovcnt; uint32_t vm_flags; };
#define VMNET_SUCCESS 1000
#define VMNET_HOST_MODE 1000
#define VMNET_SHARED_MODE 1001
#define VMNET_BRIDGED_MODE 1002
extern const char *vmnet_operation_mode_key, *vmnet_interface_id_key, *vmnet_start_address_key,
    *vmnet_end_address_key, *vmnet_subnet_mask_key, *vmnet_nat66_prefix_key,
    *vmnet_shared_interface_name_key,
    *vmnet_mac_address_key, *vmnet_mtu_key, *vmnet_max_packet_size_key;
interface_ref vmnet_start_interface(xpc_object_t, dispatch_queue_t, void (^)(vmnet_return_t, xpc_object_t));
vmnet_return_t vmnet_stop_interface(interface_ref, dispatch_queue_t, void (^)(vmnet_return_t));
vmnet_return_t vmnet_read(interface_ref, struct vmpktdesc *, int *);
vmnet_return_t vmnet_write(interface_ref, struct vmpktdesc *, int *);
xpc_object_t vmnet_copy_shared_interface_list(void);
