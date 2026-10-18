/* clankerd-owned native attachment. The VM and Ethernet relay stay unprivileged. */
#include <arpa/inet.h>
#include <dispatch/dispatch.h>
#include <errno.h>
#include <grp.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>
#include <uuid/uuid.h>
#include <xpc/xpc.h>
#ifdef CLANKERD_CROSS
#include "vmnet-abi.h"
#else
#include <vmnet/vmnet.h>
#endif

static volatile sig_atomic_t running = 1;
static void stop(int sig) { (void)sig; running = 0; }
static const char *option(int argc, char **argv, const char *key) {
    for (int i=1; i<argc; i++) if (!strcmp(argv[i],key)) return i+1<argc ? argv[i+1] : NULL;
    return NULL;
}
static int flag(int argc, char **argv, const char *key) {
    for (int i=1; i<argc; i++) if (!strcmp(argv[i],key)) return 1;
    return 0;
}
static void set_string(xpc_object_t desc, const char *key, const char *value) {
    if (value) xpc_dictionary_set_string(desc,key,value);
}
int clankerd_vmnet(int argc, char **argv) {
    if (geteuid()!=0) { fprintf(stderr,"native networking requires administrator authorization\n"); return 1; }
    if (argc == 2 && flag(argc,argv,"--list-shared-interfaces")) {
        xpc_object_t list=vmnet_copy_shared_interface_list();
        if (!list) return 1;
        for (size_t i=0;i<xpc_array_get_count(list);i++) {
            const char *name=xpc_array_get_string(list,i);
            if (name) puts(name);
        }
        xpc_release(list); return 0;
    }
    const char *keys[] = {"--socket", "--interface-id", "--operation-mode", "--start-address",
        "--end-address", "--subnet-mask", "--nat66-prefix", "--shared-interface"};
    for (int i=1; i<argc; i+=2) {
        int known=0;
        for (size_t k=0; k<sizeof(keys)/sizeof(keys[0]); k++) if (!strcmp(argv[i],keys[k])) known=1;
        if (!known || i+1>=argc) { fprintf(stderr,"invalid native attachment option: %s\n",argv[i]); return 2; }
        for (int j=1; j<i; j+=2) if (!strcmp(argv[i],argv[j])) return 2;
    }
    const char *path=option(argc,argv,"--socket"), *identity=option(argc,argv,"--interface-id"),
        *mode=option(argc,argv,"--operation-mode");
    uuid_t id;
    if (!path || strlen(path)>=sizeof(((struct sockaddr_un *)0)->sun_path) || !identity || uuid_parse(identity,id) || !mode) return 2;
    uint64_t operation=!strcmp(mode,"shared")?VMNET_SHARED_MODE:!strcmp(mode,"host")?VMNET_HOST_MODE:!strcmp(mode,"bridged")?VMNET_BRIDGED_MODE:0;
    if (!operation) return 2;
    dispatch_queue_t queue=dispatch_queue_create("io.clankerd.vmnet",NULL);
    dispatch_semaphore_t completed=dispatch_semaphore_create(0);
    xpc_object_t desc=xpc_dictionary_create(NULL,NULL,0);
    xpc_dictionary_set_uuid(desc,vmnet_interface_id_key,id);
    xpc_dictionary_set_uint64(desc,vmnet_operation_mode_key,operation);
    set_string(desc,vmnet_start_address_key,option(argc,argv,"--start-address"));
    set_string(desc,vmnet_end_address_key,option(argc,argv,"--end-address"));
    set_string(desc,vmnet_subnet_mask_key,option(argc,argv,"--subnet-mask"));
    set_string(desc,vmnet_nat66_prefix_key,option(argc,argv,"--nat66-prefix"));
    set_string(desc,vmnet_shared_interface_name_key,option(argc,argv,"--shared-interface"));
    __block vmnet_return_t status=0;
    __block xpc_object_t metadata=NULL;
    interface_ref interface=vmnet_start_interface(desc,queue,^(vmnet_return_t result,xpc_object_t info) {
        status=result; if(info) metadata=xpc_retain(info); dispatch_semaphore_signal(completed);
    });
    xpc_release(desc);
    /* Callback stack storage must remain live until completion. Process exit
     * on timeout releases its native interface rather than returning past it. */
    if(dispatch_semaphore_wait(completed,dispatch_time(DISPATCH_TIME_NOW,15*NSEC_PER_SEC))) _Exit(1);
    if (!interface || status!=VMNET_SUCCESS || !metadata) { fprintf(stderr,"vmnet start failed: %u\n",status); return 1; }
    const char *mac=xpc_dictionary_get_string(metadata,vmnet_mac_address_key);
    const char *prefix=xpc_dictionary_get_string(metadata,vmnet_nat66_prefix_key);
    uint64_t mtu=xpc_dictionary_get_uint64(metadata,vmnet_mtu_key), max=xpc_dictionary_get_uint64(metadata,vmnet_max_packet_size_key);
    int result=1, fd=-1;
    unsigned char *buffer=NULL;
    if(!mac || max<mtu+14 || max>65536) goto cleanup;
    /* Retain the authorized interface, then relinquish root before exposing
     * the Unix socket. No privileged executable is installed. */
    const char *uid=getenv("SUDO_UID"), *gid=getenv("SUDO_GID");
    if(!uid || !gid || !strtoul(uid,NULL,10) || setgroups(0,NULL) || setgid((gid_t)strtoul(gid,NULL,10)) || setuid((uid_t)strtoul(uid,NULL,10))) goto cleanup;
    fd=socket(AF_UNIX,SOCK_DGRAM,0);
    struct sockaddr_un address={0}; address.sun_family=AF_UNIX;
    strcpy(address.sun_path,path); address.sun_len=(uint8_t)(offsetof(struct sockaddr_un,sun_path)+strlen(path));
    if(fd<0 || bind(fd,(struct sockaddr *)&address,address.sun_len)) goto cleanup;
    chmod(path,0600);
    buffer=malloc(max);
    if(!buffer) goto cleanup;
    signal(SIGTERM,stop); signal(SIGINT,stop); signal(SIGHUP,stop);
    printf("{\"pid\":%ld,\"vmnet_mac_address\":\"%s\",\"vmnet_mtu\":%llu,\"vmnet_max_packet_size\":%llu",(long)getpid(),mac,(unsigned long long)mtu,(unsigned long long)max);
    if(prefix) printf(",\"vmnet_nat66_prefix\":\"%s\"",prefix);
    puts("}"); fflush(stdout);
    struct sockaddr_un peer={0}; socklen_t peer_len=0;
    result=0;
    while(running) {
        struct pollfd p={.fd=fd,.events=POLLIN};
        int polled=poll(&p,1,5);
        if(polled<0 && errno!=EINTR) { result=1; break; }
        if(polled>0 && (p.revents&POLLIN)) {
            socklen_t length=sizeof(peer);
            ssize_t size=recvfrom(fd,buffer,max,0,(struct sockaddr *)&peer,&length);
            if(size>=0) peer_len=length;
            if(size>=14) {
                struct iovec iov={buffer,(size_t)size};
                struct vmpktdesc packet={(size_t)size,&iov,1,0}; int count=1;
                if(vmnet_write(interface,&packet,&count)!=VMNET_SUCCESS) { result=1; break; }
            }
        }
        for(int batch=0;batch<64;batch++) {
            struct iovec iov={buffer,max}; struct vmpktdesc packet={max,&iov,1,0}; int count=1;
            if(vmnet_read(interface,&packet,&count)!=VMNET_SUCCESS) { result=1; running=0; break; }
            if(!count) break;
            if(peer_len) sendto(fd,buffer,packet.vm_pkt_size,0,(struct sockaddr *)&peer,peer_len);
        }
    }
cleanup:
    if(fd>=0) { close(fd); unlink(path); }
    free(buffer); xpc_release(metadata);
    status=0;
    if(vmnet_stop_interface(interface,queue,^(vmnet_return_t code) { status=code; dispatch_semaphore_signal(completed); })!=VMNET_SUCCESS) return 1;
    if(dispatch_semaphore_wait(completed,dispatch_time(DISPATCH_TIME_NOW,10*NSEC_PER_SEC))) _Exit(1);
    dispatch_release(completed); dispatch_release(queue);
    return status==VMNET_SUCCESS ? result : 1;
}
