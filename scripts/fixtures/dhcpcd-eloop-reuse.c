#include <sys/types.h>
#include <sys/socket.h>
#include <assert.h>
#include <stdio.h>
#include <unistd.h>
#include "eloop.h"
static void callback(void *ctx, unsigned short events) {(void)ctx;(void)events;}
int main(void) {
 struct eloop *loop=eloop_new(); assert(loop);
 int fd=socket(AF_INET,SOCK_DGRAM,0);assert(fd>=0);
 assert(eloop_event_add(loop,fd,ELE_READ,callback,NULL)==1);
 close(fd);
 assert(eloop_event_delete(loop,fd)==-1);
 int replacement=socket(AF_INET,SOCK_DGRAM,0);assert(replacement==fd);
 if(eloop_event_add(loop,replacement,ELE_READ,callback,NULL)!=1){perror("re-register reused fd");return 1;}
 assert(eloop_event_count(loop)==1);
 assert(eloop_event_delete(loop,replacement)==1);
 close(replacement);eloop_free(loop);puts("PASS closed-descriptor deletion and descriptor reuse");return 0;
}
