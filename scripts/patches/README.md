# Native dependency patches

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

## Prefix-capable vmnet helper

`vmnet-helper-nat66-prefix.patch` adds a validated shared-mode `--nat66-prefix`
argument to upstream vmnet-helper 0.13.0 at commit
`222c121ba31e49856c9ec6c3a14426b49d77c8fe`. The build downloads a
checksum-pinned source archive, retains its Apache-2.0 license, and builds in the
gate-owned directory. It does not replace the installed helper or its sudoers
configuration.

`scripts/m2-smoke-native-prefixes.sh` exercises real guests using this build and
persistent resource prefix selection. The separate installer places the extended
helper under `/opt/clankerd/libexec`; it does not replace `/opt/vmnet-helper`.
