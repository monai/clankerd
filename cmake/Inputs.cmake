set(alpine_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/aarch64/alpine-minirootfs-3.24.2-aarch64.tar.gz")
set(alpine_SHA256 "9bf70a7f18ea44094cbb5f70c58f9af129c8214745743db0e68e5502cc2ce773")
set(alpine_VERSION "3.24.2")
set(alpine_LICENSE "See Alpine installed package database")
set(guest_busybox_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/busybox-1.37.0-r31.apk")
set(guest_busybox_SHA256 "18e3490a6f024004b2b13dd4b54bd50ccd82894938023b2d4eab389a4edcba70")
set(guest_busybox_VERSION "1.37.0-r31")
set(guest_busybox_LICENSE "GPL-2.0-only")
set(guest_busybox_binsh_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/busybox-binsh-1.37.0-r31.apk")
set(guest_busybox_binsh_SHA256 "f3a1b2257cfa114ff20de05c665f380695385d589908bfc9af5768f39fef9ef5")
set(guest_busybox_binsh_VERSION "1.37.0-r31")
set(guest_busybox_binsh_LICENSE "GPL-2.0-only")
set(guest_dhcpcd_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/dhcpcd-10.5.2-r1.apk")
set(guest_dhcpcd_SHA256 "d34859827aba22a224f0c5624f7ea8c62ec5c80c5db302c1fc32289c9ad243fe")
set(guest_dhcpcd_VERSION "10.5.2-r1")
set(guest_dhcpcd_LICENSE "BSD-2-Clause")
set(guest_e2fsprogs_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/e2fsprogs-1.47.4-r0.apk")
set(guest_e2fsprogs_SHA256 "52fd401e79ce6b0ff7648733ba4de3135083cd0132543534e72106e7ea767a00")
set(guest_e2fsprogs_VERSION "1.47.4-r0")
set(guest_e2fsprogs_LICENSE "GPL-2.0-or-later AND LGPL-2.0-or-later AND BSD-3-Clause AND MIT")
set(guest_e2fsprogs_extra_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/e2fsprogs-extra-1.47.4-r0.apk")
set(guest_e2fsprogs_extra_SHA256 "70b0a01bf62349263a8f400809c7512e991b7db13dbebbf9b3a1cb3b3d21efcc")
set(guest_e2fsprogs_extra_VERSION "1.47.4-r0")
set(guest_e2fsprogs_extra_LICENSE "GPL-2.0-or-later AND LGPL-2.0-or-later AND BSD-3-Clause AND MIT")
set(guest_e2fsprogs_libs_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/e2fsprogs-libs-1.47.4-r0.apk")
set(guest_e2fsprogs_libs_SHA256 "52124971ae397d599aadd79263fcd68fc9e271922ccf8185b124f35fe24c60c8")
set(guest_e2fsprogs_libs_VERSION "1.47.4-r0")
set(guest_e2fsprogs_libs_LICENSE "GPL-2.0-or-later AND LGPL-2.0-or-later AND BSD-3-Clause AND MIT")
set(guest_libblkid_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/libblkid-2.42.3-r1.apk")
set(guest_libblkid_SHA256 "9d0f24976c575c53c12526b6e078edf34c980c153bc8e5f1c1053cbece5b8cfa")
set(guest_libblkid_VERSION "2.42.3-r1")
set(guest_libblkid_LICENSE "LGPL-2.1-or-later")
set(guest_libcom_err_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/libcom_err-1.47.4-r0.apk")
set(guest_libcom_err_SHA256 "a75b5955ff57046e33fc2842cfe466dc77d8e68305d5c86c6646b38a069490a2")
set(guest_libcom_err_VERSION "1.47.4-r0")
set(guest_libcom_err_LICENSE "GPL-2.0-or-later AND LGPL-2.0-or-later AND BSD-3-Clause AND MIT")
set(guest_libeconf_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/libeconf-0.8.3-r0.apk")
set(guest_libeconf_SHA256 "cf0899d49ded0a6891e3cdf1b37887f2ecd77293afe1d8f3cb8937763c83f183")
set(guest_libeconf_VERSION "0.8.3-r0")
set(guest_libeconf_LICENSE "MIT")
set(guest_libuuid_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/libuuid-2.42.3-r1.apk")
set(guest_libuuid_SHA256 "9ce20c7ffe2ccaa7c321893c10564abbca13c3f2edb82f60a35f1f68e004f86c")
set(guest_libuuid_VERSION "2.42.3-r1")
set(guest_libuuid_LICENSE "BSD-3-Clause")
set(guest_musl_URL "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/musl-1.2.6-r2.apk")
set(guest_musl_SHA256 "5e9674b7f41152fe2119093b5cb4c13eaaadb19c2d5422b2d7267913e663ee6e")
set(guest_musl_VERSION "1.2.6-r2")
set(guest_musl_LICENSE "MIT")
set(GUEST_INPUTS alpine
    guest_busybox
    guest_busybox_binsh
    guest_dhcpcd
    guest_e2fsprogs
    guest_e2fsprogs_extra
    guest_e2fsprogs_libs
    guest_libblkid
    guest_libcom_err
    guest_libeconf
    guest_libuuid
    guest_musl)
set(INPUTS ${GUEST_INPUTS})
set(license_MIT_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/MIT.txt")
set(license_MIT_SHA256 "b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5")
set(license_MIT_VERSION "SPDX-v3.27.0")
set(license_MIT_LICENSE "MIT")
set(license_BSD_2_Clause_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/BSD-2-Clause.txt")
set(license_BSD_2_Clause_SHA256 "f32fb3b417a194167cfad068223fc975ba96c5960513a10f66a3c28720aec1df")
set(license_BSD_2_Clause_VERSION "SPDX-v3.27.0")
set(license_BSD_2_Clause_LICENSE "BSD-2-Clause")
set(license_BSD_3_Clause_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/BSD-3-Clause.txt")
set(license_BSD_3_Clause_SHA256 "5a93d5831e1297ab10fe643e1a631e83be392896da14ee2951285a79012df69d")
set(license_BSD_3_Clause_VERSION "SPDX-v3.27.0")
set(license_BSD_3_Clause_LICENSE "BSD-3-Clause")
set(license_GPL_2_0_only_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/GPL-2.0-only.txt")
set(license_GPL_2_0_only_SHA256 "aaf135472f81c5b4a0dca9367e5bb5e9750032b5bebe5442b36e4c0a47430df3")
set(license_GPL_2_0_only_VERSION "SPDX-v3.27.0")
set(license_GPL_2_0_only_LICENSE "GPL-2.0-only")
set(license_GPL_2_0_or_later_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/GPL-2.0-or-later.txt")
set(license_GPL_2_0_or_later_SHA256 "aaf135472f81c5b4a0dca9367e5bb5e9750032b5bebe5442b36e4c0a47430df3")
set(license_GPL_2_0_or_later_VERSION "SPDX-v3.27.0")
set(license_GPL_2_0_or_later_LICENSE "GPL-2.0-or-later")
set(license_LGPL_2_0_or_later_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/LGPL-2.0-or-later.txt")
set(license_LGPL_2_0_or_later_SHA256 "86dc99d7e5060915ab1dfc1378b7dd351c62088bfa74067e8aa1868c6fdba7d8")
set(license_LGPL_2_0_or_later_VERSION "SPDX-v3.27.0")
set(license_LGPL_2_0_or_later_LICENSE "LGPL-2.0-or-later")
set(license_LGPL_2_1_only_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/LGPL-2.1-only.txt")
set(license_LGPL_2_1_only_SHA256 "5749785c8bdefafcb5d798270ed0a967036fe2ca63dcedade1627565dfef81d2")
set(license_LGPL_2_1_only_VERSION "SPDX-v3.27.0")
set(license_LGPL_2_1_only_LICENSE "LGPL-2.1-only")
set(license_LGPL_2_1_or_later_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/LGPL-2.1-or-later.txt")
set(license_LGPL_2_1_or_later_SHA256 "5749785c8bdefafcb5d798270ed0a967036fe2ca63dcedade1627565dfef81d2")
set(license_LGPL_2_1_or_later_VERSION "SPDX-v3.27.0")
set(license_LGPL_2_1_or_later_LICENSE "LGPL-2.1-or-later")
set(license_Apache_2_0_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/Apache-2.0.txt")
set(license_Apache_2_0_SHA256 "074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff")
set(license_Apache_2_0_VERSION "SPDX-v3.27.0")
set(license_Apache_2_0_LICENSE "Apache-2.0")
set(license_MPL_2_0_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/MPL-2.0.txt")
set(license_MPL_2_0_SHA256 "66a3107d5ad6a058aab753eaac2047ccb2ed0e39465dd0fe5844da3e300d5172")
set(license_MPL_2_0_VERSION "SPDX-v3.27.0")
set(license_MPL_2_0_LICENSE "MPL-2.0")
set(license_Zlib_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/Zlib.txt")
set(license_Zlib_SHA256 "bfb1112d49db5b1daecdfef24bd7e2f3ea0bafb33aa67aa0ab51e2bf8407c03d")
set(license_Zlib_VERSION "SPDX-v3.27.0")
set(license_Zlib_LICENSE "Zlib")
set(license_0BSD_URL "https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/0BSD.txt")
set(license_0BSD_SHA256 "e3f18c71e10d673590eb9856c1d79dd3b4b0d65404efb5e8584dbede7edd608b")
set(license_0BSD_VERSION "SPDX-v3.27.0")
set(license_0BSD_LICENSE "0BSD")
set(LICENSE_INPUTS license_MIT
    license_BSD_2_Clause
    license_BSD_3_Clause
    license_GPL_2_0_only
    license_GPL_2_0_or_later
    license_LGPL_2_0_or_later
    license_LGPL_2_1_only
    license_LGPL_2_1_or_later
    license_Apache_2_0
    license_MPL_2_0
    license_Zlib
    license_0BSD)
set(GUEST_REQUIRED bin/sh
    sbin/dhcpcd
    sbin/mke2fs
    sbin/e2fsck
    usr/sbin/resize2fs
    usr/sbin/debugfs
    lib/ld-musl-aarch64.so.1
    etc/passwd
    etc/group
    etc/shadow
    etc/mke2fs.conf
    etc/clankerd/dhcpcd.conf
    usr/lib/clankerd/network-hook)
