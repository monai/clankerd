// Native vmnet capability probe; every interface belongs to this process.
#include <arpa/inet.h>
#include <dispatch/dispatch.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <uuid/uuid.h>
#include <vmnet/vmnet.h>
#include <xpc/xpc.h>

static const char *value(xpc_object_t info, const char *key) {
    const char *text = info ? xpc_dictionary_get_string(info, key) : NULL;
    return text ? text : "";
}

int main(int argc, char **argv) {
    if (argc != 11 || geteuid() != 0) {
        fprintf(stderr, "Requires root, eight subnet octets, a ULA prefix stem and a profile\n");
        return 2;
    }
    dispatch_queue_t queue = dispatch_queue_create("clankerd-prefix-probe", NULL);
    int first, last;
    if (!strcmp(argv[10], "shared")) { first = 0; last = 4; }
    else if (!strcmp(argv[10], "host-default")) { first = 4; last = 6; }
    else if (!strcmp(argv[10], "host-custom-a")) { first = 6; last = 7; }
    else if (!strcmp(argv[10], "host-custom-b")) { first = 7; last = 8; }
    else return 2;
    interface_ref interfaces[8] = {0};
    int failed_cleanup = 0;
    for (int i = first; i < last; i++) {
        char start[32], end[32], prefix[80];
        int third = atoi(argv[i + 1]);
        if (third < 1 || third > 254) return 2;
        snprintf(start, sizeof(start), "192.168.%d.1", third);
        snprintf(end, sizeof(end), "192.168.%d.254", third);
        snprintf(prefix, sizeof(prefix), "%s:%x::", argv[9], i + 1);
        struct in6_addr parsed;
        if (inet_pton(AF_INET6, prefix, &parsed) != 1 || parsed.s6_addr[0] != 0xfd) return 2;
        uuid_t id;
        uuid_generate(id);
        xpc_object_t desc = xpc_dictionary_create(NULL, NULL, 0);
        xpc_dictionary_set_uuid(desc, vmnet_interface_id_key, id);
        xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key,
                                 i < 4 ? VMNET_SHARED_MODE : VMNET_HOST_MODE);
        xpc_dictionary_set_string(desc, vmnet_start_address_key, start);
        xpc_dictionary_set_string(desc, vmnet_end_address_key, end);
        xpc_dictionary_set_string(desc, vmnet_subnet_mask_key, "255.255.255.0");
        if ((i >= 2 && i < 4) || i >= 6) xpc_dictionary_set_string(desc, vmnet_nat66_prefix_key, prefix);
        dispatch_semaphore_t completed = dispatch_semaphore_create(0);
        __block vmnet_return_t result = 0;
        __block xpc_object_t metadata = NULL;
        interfaces[i] = vmnet_start_interface(desc, queue, ^(vmnet_return_t status, xpc_object_t info) {
            result = status;
            if (info) metadata = xpc_retain(info);
            dispatch_semaphore_signal(completed);
        });
        // Callback storage remains live until completion; timeout terminates the
        // process, which owns these temporary native interface references.
        if (dispatch_semaphore_wait(completed, dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC))) {
            fprintf(stderr, "Native start callback timed out for case %d\n", i);
            _Exit(2);
        }
        printf("{\"event\":\"start\",\"case\":%d,\"mode\":\"%s\",\"requested_prefix\":\"%s\",\"status\":%d,\"success\":%s,\"prefix\":\"%s\",\"start\":\"%s\"}\n",
               i, i < 4 ? "shared" : "host-only", ((i >= 2 && i < 4) || i >= 6) ? prefix : "",
               (int)result, result == VMNET_SUCCESS ? "true" : "false",
               value(metadata, vmnet_nat66_prefix_key), value(metadata, vmnet_start_address_key));
        fflush(stdout);
        if (result != VMNET_SUCCESS) interfaces[i] = NULL;
        if (metadata) {
            char *description = xpc_copy_description(metadata);
            fprintf(stderr, "case %d metadata: %s\n", i, description ? description : "(none)");
            free(description);
            xpc_release(metadata);
        }
        xpc_release(desc);
        dispatch_release(completed);
    }
    // All successful cases coexist before teardown, so sequential reuse cannot
    // masquerade as simultaneous support for different prefixes.
    for (int i = last - 1; i >= first; i--) {
        if (!interfaces[i]) continue;
        dispatch_semaphore_t completed = dispatch_semaphore_create(0);
        __block vmnet_return_t result = 0;
        vmnet_return_t submitted = vmnet_stop_interface(interfaces[i], queue, ^(vmnet_return_t status) {
            result = status;
            dispatch_semaphore_signal(completed);
        });
        if (submitted != VMNET_SUCCESS) {
            failed_cleanup = 1;
        } else if (dispatch_semaphore_wait(completed, dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC))) {
            fprintf(stderr, "Native stop callback timed out for case %d\n", i);
            _Exit(2);
        } else if (result != VMNET_SUCCESS) {
            failed_cleanup = 1;
        }
        printf("{\"event\":\"stop\",\"case\":%d,\"submitted\":%d,\"status\":%d}\n", i, (int)submitted, (int)result);
        dispatch_release(completed);
    }
    dispatch_release(queue);
    return failed_cleanup;
}
