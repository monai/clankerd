#include <dispatch/dispatch.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <xpc/xpc.h>

#define SERVICE "io.clankerd.network"
typedef void (*handler_t)(const char*, unsigned int, int, unsigned long long, char**, int*);
typedef void (*disconnect_t)(unsigned long long);

void* clankerd_xpc_connect(void) {
  xpc_connection_t connection =
      xpc_connection_create_mach_service(SERVICE, NULL, XPC_CONNECTION_MACH_SERVICE_PRIVILEGED);
  if (!connection) return NULL;
  xpc_connection_set_event_handler(connection, ^(xpc_object_t event) {
    (void)event;
  });
  xpc_connection_resume(connection);
  return connection;
}
void clankerd_xpc_close(void* connection) {
  xpc_connection_cancel(connection);
  xpc_release(connection);
}
int clankerd_xpc_request(void* connection, const char* json, char** output, int* fd) {
  xpc_object_t message = xpc_dictionary_create(NULL, NULL, 0);
  xpc_dictionary_set_string(message, "request", json);
  xpc_object_t reply = xpc_connection_send_message_with_reply_sync(connection, message);
  xpc_release(message);
  if (!reply || xpc_get_type(reply) != XPC_TYPE_DICTIONARY) {
    if (reply) xpc_release(reply);
    return -1;
  }
  const char* text = xpc_dictionary_get_string(reply, "response");
  *output = text ? strdup(text) : NULL;
  *fd = xpc_dictionary_dup_fd(reply, "packet_fd");
  xpc_release(reply);
  return *output ? 0 : -1;
}
void clankerd_xpc_serve(handler_t handler, disconnect_t disconnected, void (*shutdown)(void)) {
  signal(SIGTERM, SIG_IGN);
  dispatch_source_t termination = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGTERM, 0,
                                                         dispatch_get_global_queue(DISPATCH_QUEUE_PRIORITY_DEFAULT, 0));
  dispatch_source_set_event_handler(termination, ^{
    shutdown();
    exit(0);
  });
  dispatch_resume(termination);
  xpc_connection_t listener =
      xpc_connection_create_mach_service(SERVICE, dispatch_get_main_queue(), XPC_CONNECTION_MACH_SERVICE_LISTENER);
  if (!listener) exit(1);
  __block unsigned long long next = 0;
  xpc_connection_set_event_handler(listener, ^(xpc_object_t event) {
    if (xpc_get_type(event) != XPC_TYPE_CONNECTION) return;
    xpc_connection_t peer = (xpc_connection_t)event;
    unsigned long long identity = ++next;
    unsigned int uid = xpc_connection_get_euid(peer);
    int pid = xpc_connection_get_pid(peer);
    xpc_connection_set_event_handler(peer, ^(xpc_object_t request) {
      if (xpc_get_type(request) == XPC_TYPE_ERROR) {
        disconnected(identity);
        return;
      }
      if (xpc_get_type(request) != XPC_TYPE_DICTIONARY) return;
      const char* json = xpc_dictionary_get_string(request, "request");
      if (!json) return;
      xpc_object_t reply = xpc_dictionary_create_reply(request);
      if (!reply) return;
      char* output = NULL;
      int fd = -1;
      handler(json, uid, pid, identity, &output, &fd);
      xpc_dictionary_set_string(reply, "response", output ? output : "{\"error\":\"invalid request\"}");
      if (fd >= 0) {
        xpc_dictionary_set_fd(reply, "packet_fd", fd);
        close(fd);
      }
      xpc_connection_send_message(peer, reply);
      xpc_release(reply);
      free(output);
    });
    xpc_connection_resume(peer);
  });
  xpc_connection_resume(listener);
  dispatch_main();
}
