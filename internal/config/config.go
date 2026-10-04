// Package config layers flags, CLANKERD_* environment variables, project, user and system TOML
// files and defaults (highest first) into one Config, and resolves where files live.
package config

import (
	"flag"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/knadh/koanf/parsers/toml/v2"
	"github.com/knadh/koanf/providers/confmap"
	"github.com/knadh/koanf/providers/file"
	"github.com/knadh/koanf/v2"
)

// Smol describes the VM that `smol up` creates.
type Smol struct {
	Image      string
	CPUs       int
	Mem        int
	Storage    int
	Net        bool
	NetBackend string
	User       string
	Volumes    []string
	Env        []string
	Init       []string
}

type Config struct {
	VM             string
	Slots          int
	AppPortBase    int
	CDPPortBase    int
	ChromePortBase int
	RelayBind      []string // addresses for the daemon CDP relay, IPv4 and IPv6 alike
	MDNSSubnets    []netip.Prefix
	MDNSGroup4     string
	MDNSGroup6     string
	ChromeBin      string
	HostAddr       string // how the VM reaches the host; empty = default gateway
	GuestDir       string // the VM's temporary directory for relay pidfiles
	GuestBin       string // development build of clankerctl as mounted in the VM; empty = none
	LogLevel       string
	Smol           Smol
	Dirs           Dirs
}

type key struct{ toml, env, flag string }

var keys = []key{
	{"vm.name", "CLANKERD_VM", "vm"},
	{"ports.slots", "CLANKERD_SLOTS", "slots"},
	{"ports.app_base", "CLANKERD_APP_PORT_BASE", "app-port-base"},
	{"ports.cdp_base", "CLANKERD_CDP_PORT_BASE", "cdp-port-base"},
	{"ports.chrome_base", "CLANKERD_CHROME_PORT_BASE", "chrome-port-base"},
	{"ports.relay_bind", "CLANKERD_RELAY_BIND", "relay-bind"},
	{"mdns.subnets", "CLANKERD_MDNS_SUBNETS", "mdns-subnets"},
	{"mdns.group4", "CLANKERD_MDNS_GROUP4", "mdns-group4"},
	{"mdns.group6", "CLANKERD_MDNS_GROUP6", "mdns-group6"},
	{"chrome.bin", "CLANKERD_CHROME_BIN", "chrome-bin"},
	{"guest.host_addr", "CLANKERD_HOST_ADDR", "host-addr"},
	{"guest.dir", "CLANKERD_GUEST_DIR", "guest-dir"},
	{"guest.bin", "CLANKERD_GUEST_BIN", "guest-bin"},
	{"log.level", "CLANKERD_LOG_LEVEL", "log-level"},
}

func defaults() map[string]any {
	return map[string]any{
		"vm.name":           "sandbox",
		"ports.slots":       10,
		"ports.app_base":    4000,
		"ports.cdp_base":    9222,
		"ports.chrome_base": 19222,
		"ports.relay_bind":  []string{"127.0.0.1", "::1"},
		"mdns.subnets":      []string{},
		"mdns.group4":       "224.0.0.251:5353",
		"mdns.group6":       "[ff02::fb]:5353",
		"chrome.bin":        "",
		"guest.host_addr":   "",
		"guest.dir":         "/tmp/clankerd",
		"guest.bin":         "/mnt/workspace/sandbox/clankerd/build/linux-arm64/clankerctl",
		"log.level":         "info",

		"smol.image":       "ghcr.io/monai/clankers:slim",
		"smol.cpus":        4,
		"smol.mem":         8192,
		"smol.storage":     4,
		"smol.net":         true,
		"smol.net_backend": "virtio-net",
		"smol.user":        "agent",
		"smol.volumes":     []string{".:/mnt/workspace"},
		"smol.env":         []string{},
		"smol.init": []string{
			`sh -c 'groupmod -o -g $HOST_GID agent && usermod -o -u $HOST_UID agent && { mountpoint -q /home/agent || chown -R $HOST_UID:$HOST_GID /home/agent; } && chown $HOST_UID:$HOST_GID /workspace'`,
			"mkdir -p /storage/docker",
		},
	}
}

// Flags holds the command-line overrides. Register them on any FlagSet with AddFlags.
type Flags struct {
	fs   *flag.FlagSet
	home *string
	file *string
	vals map[string]*string
}

func AddFlags(fs *flag.FlagSet) *Flags {
	f := &Flags{fs: fs, vals: map[string]*string{}}
	f.home = fs.String("home", "", "directory holding all config and state (env CLANKERD_HOME)")
	f.file = fs.String("config", "", "extra TOML config file (env CLANKERD_CONFIG)")
	for _, k := range keys {
		f.vals[k.toml] = fs.String(k.flag, "", "overrides "+k.toml+" (env "+k.env+")")
	}
	return f
}

// Args re-emits the flags that were set, so a spawned daemon resolves the same configuration.
func (f *Flags) Args() []string {
	var out []string
	f.fs.Visit(func(fl *flag.Flag) {
		if _, ok := f.vals[tomlKeyForFlag(fl.Name)]; ok || fl.Name == "home" || fl.Name == "config" {
			out = append(out, "--"+fl.Name+"="+fl.Value.String())
		}
	})
	return out
}

func tomlKeyForFlag(name string) string {
	for _, k := range keys {
		if k.flag == name {
			return k.toml
		}
	}
	return ""
}

func (f *Flags) set() map[string]any {
	m := map[string]any{}
	f.fs.Visit(func(fl *flag.Flag) {
		if k := tomlKeyForFlag(fl.Name); k != "" {
			m[k] = fl.Value.String()
		}
	})
	return m
}

func (f *Flags) visited(name string) bool {
	seen := false
	f.fs.Visit(func(fl *flag.Flag) { seen = seen || fl.Name == name })
	return seen
}

// Load resolves the configuration for a process running in cwd. f may be nil.
func Load(f *Flags, cwd string) (*Config, error) {
	home := os.Getenv("CLANKERD_HOME")
	extra, hasExtra := os.LookupEnv("CLANKERD_CONFIG")
	if f != nil {
		if f.visited("home") {
			home = *f.home
		}
		if f.visited("config") {
			extra, hasExtra = *f.file, true
		}
	}
	lay, files := locate(home, cwd)
	if hasExtra && extra != "" {
		files = append(files, extra)
	}

	k := koanf.New(".")
	if err := k.Load(confmap.Provider(defaults(), "."), nil); err != nil {
		return nil, err
	}
	for _, p := range files {
		if _, err := os.Stat(p); err != nil {
			if p == extra {
				return nil, fmt.Errorf("config file %s: %w", p, err)
			}
			continue
		}
		if err := k.Load(file.Provider(p), toml.Parser()); err != nil {
			return nil, fmt.Errorf("config file %s: %w", p, err)
		}
	}
	env := map[string]any{}
	for _, kk := range keys {
		if v, ok := os.LookupEnv(kk.env); ok {
			env[kk.toml] = v
		}
	}
	if err := k.Load(confmap.Provider(env, "."), nil); err != nil {
		return nil, err
	}
	if f != nil {
		if err := k.Load(confmap.Provider(f.set(), "."), nil); err != nil {
			return nil, err
		}
	}
	return build(k, lay, cwd)
}

func str(k *koanf.Koanf, key string) string {
	if !k.Exists(key) {
		return ""
	}
	return fmt.Sprint(k.Get(key))
}

func list(k *koanf.Koanf, key string) []string {
	switch v := k.Get(key).(type) {
	case nil:
		return nil
	case string:
		var out []string
		for _, s := range strings.Split(v, ",") {
			if s = strings.TrimSpace(s); s != "" {
				out = append(out, s)
			}
		}
		return out
	case []string:
		return v
	case []any:
		var out []string
		for _, e := range v {
			out = append(out, fmt.Sprint(e))
		}
		return out
	default:
		return []string{fmt.Sprint(v)}
	}
}

func build(k *koanf.Koanf, lay layout, cwd string) (*Config, error) {
	var errs []string
	num := func(key string) int {
		n, err := strconv.Atoi(strings.TrimSpace(str(k, key)))
		if err != nil {
			errs = append(errs, fmt.Sprintf("%s: %q is not a number", key, str(k, key)))
		}
		return n
	}
	c := &Config{
		VM:             str(k, "vm.name"),
		Slots:          num("ports.slots"),
		AppPortBase:    num("ports.app_base"),
		CDPPortBase:    num("ports.cdp_base"),
		ChromePortBase: num("ports.chrome_base"),
		RelayBind:      list(k, "ports.relay_bind"),
		MDNSGroup4:     str(k, "mdns.group4"),
		MDNSGroup6:     str(k, "mdns.group6"),
		ChromeBin:      str(k, "chrome.bin"),
		HostAddr:       str(k, "guest.host_addr"),
		GuestDir:       str(k, "guest.dir"),
		GuestBin:       str(k, "guest.bin"),
		LogLevel:       str(k, "log.level"),
		Smol: Smol{
			Image:      str(k, "smol.image"),
			CPUs:       num("smol.cpus"),
			Mem:        num("smol.mem"),
			Storage:    num("smol.storage"),
			Net:        str(k, "smol.net") == "true",
			NetBackend: str(k, "smol.net_backend"),
			User:       str(k, "smol.user"),
			Volumes:    list(k, "smol.volumes"),
			Env:        list(k, "smol.env"),
			Init:       list(k, "smol.init"),
		},
	}
	for _, s := range list(k, "mdns.subnets") {
		p, err := netip.ParsePrefix(s)
		if err != nil {
			errs = append(errs, fmt.Sprintf("invalid subnet %q (want CIDR such as 192.168.1.0/24 or fd00::/8)", s))
			continue
		}
		c.MDNSSubnets = append(c.MDNSSubnets, p.Masked())
	}
	if len(errs) > 0 {
		return nil, fmt.Errorf("%s", strings.Join(errs, "; "))
	}
	if c.VM == "" || strings.ContainsAny(c.VM, "/\\ \t") {
		return nil, fmt.Errorf("invalid vm name %q", c.VM)
	}
	if c.Slots < 1 || c.Slots > 1000 {
		return nil, fmt.Errorf("ports.slots must be between 1 and 1000, got %d", c.Slots)
	}
	for name, p := range map[string]int{"app_base": c.AppPortBase, "cdp_base": c.CDPPortBase, "chrome_base": c.ChromePortBase} {
		if p < 1 || p+c.Slots-1 > 65535 {
			return nil, fmt.Errorf("ports.%s %d with %d slots is outside 1-65535", name, p, c.Slots)
		}
	}
	d, err := resolveDirs(lay, c.VM)
	if err != nil {
		return nil, err
	}
	c.Dirs = d
	for i, v := range c.Smol.Volumes { // relative host paths are relative to where `smol up` runs
		if host, rest, ok := strings.Cut(v, ":"); ok && !filepath.IsAbs(host) {
			c.Smol.Volumes[i] = filepath.Join(cwd, host) + ":" + rest
		}
	}
	return c, nil
}
