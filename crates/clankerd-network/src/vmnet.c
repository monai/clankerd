#include <dispatch/dispatch.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <uuid/uuid.h>
#include <xpc/xpc.h>
#ifdef CLANKERD_CROSS
#include "vmnet-abi.h"
#else
#include <vmnet/vmnet.h>
#endif

typedef struct {
    interface_ref interface;
    dispatch_queue_t queue;
    size_t max_packet;
} clankerd_interface;

static void set_string(xpc_object_t desc, const char *key, const char *value) {
    if (value) xpc_dictionary_set_string(desc, key, value);
}
int clankerd_vmnet_stop(clankerd_interface *handle) {
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    __block vmnet_return_t status = 0;
    vmnet_return_t result = vmnet_stop_interface(handle->interface, handle->queue,
        ^(vmnet_return_t code) { status = code; dispatch_semaphore_signal(done); });
    if (result == VMNET_SUCCESS && dispatch_semaphore_wait(done,
        dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC))) _Exit(1);
    dispatch_release(done);
    dispatch_release(handle->queue);
    free(handle);
    return result == VMNET_SUCCESS && status == VMNET_SUCCESS ? 0 : -1;
}
clankerd_interface *clankerd_vmnet_create(const char *identity, const char *mode,
    const char *start, const char *end, const char *mask, const char *prefix,
    const char *shared, char **actual_prefix) {
    uuid_t id;
    if (uuid_parse(identity, id)) return NULL;
    uint64_t operation = !strcmp(mode, "shared") ? VMNET_SHARED_MODE :
        !strcmp(mode, "host") ? VMNET_HOST_MODE :
        !strcmp(mode, "bridged") ? VMNET_BRIDGED_MODE : 0;
    if (!operation) return NULL;
    clankerd_interface *handle = calloc(1, sizeof(*handle));
    if (!handle) return NULL;
    handle->queue = dispatch_queue_create("io.clankerd.vmnet", NULL);
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    xpc_object_t desc = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_uuid(desc, vmnet_interface_id_key, id);
    xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, operation);
    set_string(desc, vmnet_start_address_key, start);
    set_string(desc, vmnet_end_address_key, end);
    set_string(desc, vmnet_subnet_mask_key, mask);
    set_string(desc, vmnet_nat66_prefix_key, prefix);
    set_string(desc, vmnet_shared_interface_name_key, shared);
    __block vmnet_return_t status = 0;
    __block xpc_object_t metadata = NULL;
    handle->interface = vmnet_start_interface(desc, handle->queue,
        ^(vmnet_return_t code, xpc_object_t info) {
            status = code;
            if (info) metadata = xpc_retain(info);
            dispatch_semaphore_signal(done);
        });
    xpc_release(desc);
    if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC))) _Exit(1);
    dispatch_release(done);
    if (!handle->interface || status != VMNET_SUCCESS || !metadata) {
        if (metadata) xpc_release(metadata);
        if (handle->interface) clankerd_vmnet_stop(handle);
        else { dispatch_release(handle->queue); free(handle); }
        return NULL;
    }
    uint64_t mtu = xpc_dictionary_get_uint64(metadata, vmnet_mtu_key);
    handle->max_packet = xpc_dictionary_get_uint64(metadata, vmnet_max_packet_size_key);
    const char *assigned = xpc_dictionary_get_string(metadata, vmnet_nat66_prefix_key);
    *actual_prefix = assigned ? strdup(assigned) : NULL;
    xpc_release(metadata);
    if (mtu < 576 || mtu > 65535 || handle->max_packet < mtu + 14 || handle->max_packet > 65536) {
        free(*actual_prefix); *actual_prefix = NULL;
        clankerd_vmnet_stop(handle);
        return NULL;
    }
    return handle;
}
int clankerd_vmnet_read(clankerd_interface *handle, unsigned char *buffer, size_t *size) {
    struct iovec iov = { buffer, handle->max_packet };
    struct vmpktdesc packet = { handle->max_packet, &iov, 1, 0 };
    int count = 1;
    vmnet_return_t status = vmnet_read(handle->interface, &packet, &count);
    if (status != VMNET_SUCCESS) {
        fprintf(stderr, "clankerd-network: vmnet_read status=%u count=%d\n", status, count);
        return -1;
    }
    *size = count ? packet.vm_pkt_size : 0;
    return count;
}
int clankerd_vmnet_write(clankerd_interface *handle, unsigned char *buffer, size_t size) {
    if (size < 14 || size > handle->max_packet) {
        fprintf(stderr, "clankerd-network: vmnet_write invalid size=%zu max=%zu\n", size, handle->max_packet);
        return -1;
    }
    struct iovec iov = { buffer, size };
    struct vmpktdesc packet = { size, &iov, 1, 0 };
    int count = 1;
    vmnet_return_t status = vmnet_write(handle->interface, &packet, &count);
    if (status != VMNET_SUCCESS || count != 1) {
        fprintf(stderr, "clankerd-network: vmnet_write status=%u count=%d size=%zu max=%zu\n",
            status, count, size, handle->max_packet);
        return -1;
    }
    return 0;
}
char *clankerd_vmnet_list(void) {
    xpc_object_t list = vmnet_copy_shared_interface_list();
    if (!list) return NULL;
    size_t length = 1;
    for (size_t i = 0; i < xpc_array_get_count(list); i++) {
        const char *name = xpc_array_get_string(list, i);
        if (name) length += strlen(name) + 1;
    }
    char *output = calloc(length, 1);
    if (output) for (size_t i = 0; i < xpc_array_get_count(list); i++) {
        const char *name = xpc_array_get_string(list, i);
        if (name) { strcat(output, name); strcat(output, "\n"); }
    }
    xpc_release(list);
    return output;
}
