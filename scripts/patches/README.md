# dhcpcd patches

`dhcpcd-eloop-retire-closed-fd.patch` backports the event-deletion cleanup in
[upstream eloop.c](https://github.com/NetworkConfiguration/dhcpcd/blob/master/src/eloop.c)
to pinned dhcpcd 10.3.0. Kernel deletion errors still reach the caller, but the
userspace entry is retired so a reused descriptor is registered with epoll again.

The Linux regression closes a registered socket before event deletion, then
reuses its descriptor and requires registration to succeed with one active event:

```sh
scripts/test-dhcpcd-eloop.sh build/dhcpcd/src-10.3.0-aarch64-linux-musl
```

The unpatched source fails with `No such file or directory`; the patched source
passes. This reproduces the stale-event failure class. The Mac recovery gate is
still required to verify carrier recovery through the actual VM attachment.
