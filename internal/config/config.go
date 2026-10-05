// Package config layers flags, CLANKERD_* environment variables, project, user and system TOML
// files and defaults (highest first) into one Config, and resolves where files live.
package config

import (
	"encoding/json"
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
	Cmd        []string
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
	LogLevel       string
	Smol           Smol
	Dirs           Dirs
}

type kind int

const (
	scalar kind = iota
	boolean
	csv   // list; a flag or env value may also hold comma-separated items
	items // list; every flag or JSON array element is one item
)

type key struct {
	toml, env, flag string
	kind            kind
}

var keys = []key{
	{"vm.name", "CLANKERD_VM", "vm", scalar},
	{"ports.slots", "CLANKERD_SLOTS", "slots", scalar},
	{"ports.app_base", "CLANKERD_APP_PORT_BASE", "app-port-base", scalar},
	{"ports.cdp_base", "CLANKERD_CDP_PORT_BASE", "cdp-port-base", scalar},
	{"ports.chrome_base", "CLANKERD_CHROME_PORT_BASE", "chrome-port-base", scalar},
	{"ports.relay_bind", "CLANKERD_RELAY_BIND", "relay-bind", csv},
	{"mdns.subnets", "CLANKERD_MDNS_SUBNETS", "mdns-subnets", csv},
	{"mdns.group4", "CLANKERD_MDNS_GROUP4", "mdns-group4", scalar},
	{"mdns.group6", "CLANKERD_MDNS_GROUP6", "mdns-group6", scalar},
	{"chrome.bin", "CLANKERD_CHROME_BIN", "chrome-bin", scalar},
	{"guest.host_addr", "CLANKERD_HOST_ADDR", "host-addr", scalar},
	{"guest.dir", "CLANKERD_GUEST_DIR", "guest-dir", scalar},
	{"log.level", "CLANKERD_LOG_LEVEL", "log-level", scalar},
	{"smol.image", "CLANKERD_SMOL_IMAGE", "smol-image", scalar},
	{"smol.cpus", "CLANKERD_SMOL_CPUS", "smol-cpus", scalar},
	{"smol.mem", "CLANKERD_SMOL_MEM", "smol-mem", scalar},
	{"smol.storage", "CLANKERD_SMOL_STORAGE", "smol-storage", scalar},
	{"smol.net", "CLANKERD_SMOL_NET", "smol-net", boolean},
	{"smol.net_backend", "CLANKERD_SMOL_NET_BACKEND", "smol-net-backend", scalar},
	{"smol.user", "CLANKERD_SMOL_USER", "smol-user", scalar},
	{"smol.volumes", "CLANKERD_SMOL_VOLUMES", "smol-volumes", items},
	{"smol.env", "CLANKERD_SMOL_ENV", "smol-env", items},
	{"smol.init", "CLANKERD_SMOL_INIT", "smol-init", items},
	{"smol.cmd", "CLANKERD_SMOL_CMD", "smol-cmd", items},
}

func defaults() map[string]any {
	return map[string]any{
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
		"log.level":         "info",
	}
}

// Flags holds the command-line overrides. Register them on any FlagSet with AddFlags.
type Flags struct {
	fs   *flag.FlagSet
	home *string
	file *string
	vals map[string]flag.Value
}

// listValue is a repeatable flag.
type listValue struct{ items []string }

func (l *listValue) String() string     { return strings.Join(l.items, ",") }
func (l *listValue) Set(v string) error { l.items = append(l.items, v); return nil }

// boolValue lets `--smol-net` stand alone while keeping `--smol-net=false` possible.
type boolValue struct{ v string }

func (b *boolValue) String() string     { return b.v }
func (b *boolValue) Set(v string) error { b.v = v; return nil }
func (b *boolValue) IsBoolFlag() bool   { return true }

type stringValue struct{ v string }

func (s *stringValue) String() string     { return s.v }
func (s *stringValue) Set(v string) error { s.v = v; return nil }

func AddFlags(fs *flag.FlagSet) *Flags {
	f := &Flags{fs: fs, vals: map[string]flag.Value{}}
	f.home = fs.String("home", "", "directory holding all config and state (env CLANKERD_HOME)")
	f.file = fs.String("config", "", "extra TOML config file (env CLANKERD_CONFIG)")
	for _, k := range keys {
		var v flag.Value = &stringValue{}
		switch k.kind {
		case boolean:
			v = &boolValue{}
		case csv, items:
			v = &listValue{}
		}
		f.vals[k.toml] = v
		usage := "overrides " + k.toml + " (env " + k.env + ")"
		if k.kind == csv || k.kind == items {
			usage += "; repeatable"
		}
		fs.Var(v, k.flag, usage)
	}
	return f
}

// Args re-emits the flags that were set, so a spawned daemon resolves the same configuration.
func (f *Flags) Args() []string {
	var out []string
	f.fs.Visit(func(fl *flag.Flag) {
		if l, ok := fl.Value.(*listValue); ok {
			for _, it := range l.items {
				out = append(out, "--"+fl.Name+"="+it)
			}
			return
		}
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

func keyFor(tomlKey string) key {
	for _, k := range keys {
		if k.toml == tomlKey {
			return k
		}
	}
	return key{}
}

// splitCSV splits comma-separated items and drops empty ones.
func splitCSV(in []string) []string {
	out := []string{}
	for _, v := range in {
		for _, s := range strings.Split(v, ",") {
			if s = strings.TrimSpace(s); s != "" {
				out = append(out, s)
			}
		}
	}
	return out
}

func (f *Flags) set() map[string]any {
	m := map[string]any{}
	f.fs.Visit(func(fl *flag.Flag) {
		k := tomlKeyForFlag(fl.Name)
		if k == "" {
			return
		}
		switch l := fl.Value.(type) {
		case *listValue:
			if keyFor(k).kind == csv {
				m[k] = splitCSV(l.items)
			} else {
				m[k] = append([]string{}, l.items...)
			}
		default:
			m[k] = fl.Value.String()
		}
	})
	return m
}

// envValue reads a list from an environment variable: a JSON array, or else one item
// (comma separated for the csv keys).
func envValue(k key, v string) (any, error) {
	if k.kind != csv && k.kind != items {
		return v, nil
	}
	if t := strings.TrimSpace(v); strings.HasPrefix(t, "[") {
		var l []string
		if err := json.Unmarshal([]byte(t), &l); err != nil {
			return nil, fmt.Errorf("%s: want a JSON array of strings: %w", k.env, err)
		}
		return l, nil
	}
	if k.kind == csv {
		return splitCSV([]string{v}), nil
	}
	if v == "" {
		return []string{}, nil
	}
	return []string{v}, nil
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
			ev, err := envValue(kk, v)
			if err != nil {
				return nil, err
			}
			env[kk.toml] = ev
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
	optNum := func(key string) int {
		if !k.Exists(key) {
			return 0
		}
		return num(key)
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
		LogLevel:       str(k, "log.level"),
		Smol: Smol{
			Image:      str(k, "smol.image"),
			CPUs:       optNum("smol.cpus"),
			Mem:        optNum("smol.mem"),
			Storage:    optNum("smol.storage"),
			Net:        str(k, "smol.net") == "true",
			NetBackend: str(k, "smol.net_backend"),
			User:       str(k, "smol.user"),
			Volumes:    list(k, "smol.volumes"),
			Env:        list(k, "smol.env"),
			Init:       list(k, "smol.init"),
			Cmd:        list(k, "smol.cmd"),
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
	if c.VM == "" {
		return nil, fmt.Errorf("vm.name is required")
	}
	if strings.ContainsAny(c.VM, "/\\ \t") {
		return nil, fmt.Errorf("vm.name %q must not contain slashes or whitespace", c.VM)
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
	for i, v := range c.Smol.Volumes {
		if host, rest, ok := strings.Cut(v, ":"); ok && !filepath.IsAbs(host) {
			c.Smol.Volumes[i] = filepath.Join(cwd, host) + ":" + rest
		}
	}
	return c, nil
}
