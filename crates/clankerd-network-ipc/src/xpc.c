// Blocks-based libXPC and libdispatch callbacks. Message contents belong to Rust.
#include <dispatch/dispatch.h>
#include <signal.h>
#include <stdlib.h>
#include <xpc/xpc.h>

#define SERVICE "io.clankerd.network"
typedef bool (*handler_t)(xpc_object_t, xpc_object_t, unsigned int, int, unsigned long long, void*);
typedef void (*disconnect_t)(unsigned long long, void*);

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
void clankerd_xpc_serve(void* context, handler_t handler, disconnect_t disconnected, void (*shutdown)(void*)) {
  signal(SIGTERM, SIG_IGN);
  dispatch_source_t termination = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGTERM, 0,
                                                         dispatch_get_global_queue(DISPATCH_QUEUE_PRIORITY_DEFAULT, 0));
  dispatch_source_set_event_handler(termination, ^{
    shutdown(context);
    exit(0);
  });
  dispatch_resume(termination);
  xpc_connection_t listener =
      xpc_connection_create_mach_service(SERVICE, dispatch_get_main_queue(), XPC_CONNECTION_MACH_SERVICE_LISTENER);
  if (!listener) exit(1);
  // Listener events run serially on the main queue; only this callback mutates next.
  __block unsigned long long next = 0;
  xpc_connection_set_event_handler(listener, ^(xpc_object_t event) {
    if (xpc_get_type(event) != XPC_TYPE_CONNECTION) return;
    xpc_connection_t peer = (xpc_connection_t)event;
    unsigned long long identity = ++next;
    unsigned int uid = xpc_connection_get_euid(peer);
    int pid = xpc_connection_get_pid(peer);
    xpc_connection_set_event_handler(peer, ^(xpc_object_t request) {
      if (xpc_get_type(request) == XPC_TYPE_ERROR) {
        disconnected(identity, context);
        return;
      }
      if (xpc_get_type(request) != XPC_TYPE_DICTIONARY) return;
      // Preserve XPC's reply association rather than creating an unrelated dictionary.
      xpc_object_t reply = xpc_dictionary_create_reply(request);
      if (!reply) return;
      if (handler(request, reply, uid, pid, identity, context)) {
        xpc_connection_send_message(peer, reply);
      } else {
        xpc_connection_cancel(peer);
      }
      xpc_release(reply);
    });
    xpc_connection_resume(peer);
  });
  xpc_connection_resume(listener);
  dispatch_main();
}

// Serde pulls map entries one at a time; capture keys without cloning their values.
xpc_object_t clankerd_xpc_dictionary_keys(xpc_object_t dictionary) {
  xpc_object_t keys = xpc_array_create(NULL, 0);
  xpc_dictionary_apply(dictionary, ^bool(const char* key, xpc_object_t value) {
    (void)value;
    xpc_object_t string = xpc_string_create(key);
    xpc_array_append_value(keys, string);
    xpc_release(string);
    return true;
  });
  return keys;
}
