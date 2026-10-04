package e2e

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/miekg/dns"
)

func contains(t *testing.T, s string, subs ...string) {
	t.Helper()
	for _, sub := range subs {
		if !strings.Contains(s, sub) {
			t.Errorf("missing %q in:\n%s", sub, s)
		}
	}
}

func contrib(t *testing.T) string {
	p, err := filepath.Abs("../../contrib/clankers.toml")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func TestSmolUpDownStatus(t *testing.T) {
	r := newRig(t)
	r.env["CLANKERD_CONFIG"] = contrib(t)
	if out := r.ok("smol", "status"); !strings.Contains(out, "daemon: stopped") || !strings.Contains(out, "vm: missing") {
		t.Fatalf("status before up: %s", out)
	}
	r.up()
	calls := strings.Join(r.smolCalls(), "\n")
	contains(t, calls,
		"machine create", "--name sandbox",
		fmt.Sprintf("-p %d-%d:%d-%d", r.appBase, r.appBase+slots-1, r.appBase, r.appBase+slots-1),
		"--mount-socket "+r.sock()+":/run/clankerd/ctl.sock",
		"--image ghcr.io/monai/clankers:slim", "--user agent", "--env HOST_UID=", "--env HOME=/home/agent",
		"--init", "machine start")
	for _, flag := range []string{"--cpus", "--mem", "--storage", "--net"} {
		if strings.Contains(calls, flag) {
			t.Errorf("contrib profile passes %s; smolvm's default should apply", flag)
		}
	}
	if strings.Contains(calls, "{{") {
		t.Errorf("placeholders not expanded:\n%s", calls)
	}
	out := r.ok("smol", "status")
	contains(t, out, "daemon: running", "vm: running")
	if !alive(r.daemonPID()) {
		t.Fatal("daemon not alive")
	}

	r.up()
	if n := strings.Count(strings.Join(r.smolCalls(), "\n"), "machine create"); n != 1 {
		t.Fatalf("create ran %d times", n)
	}

	r.acquire("shop")
	pid := r.daemonPID()
	r.ok("smol", "down")
	calls = strings.Join(r.smolCalls(), "\n")
	contains(t, calls, "machine stop", "machine delete")
	waitFor(t, "daemon exit", func() bool { return !alive(pid) })
	contains(t, r.ok("smol", "status"), "daemon: stopped", "vm: missing")
	r.ok("smol", "down")
}

func TestSmolUpRejectsUnknownTemplateFields(t *testing.T) {
	r := newRig(t)
	os.WriteFile(filepath.Join(r.home, "config.toml"), []byte("[smol]\nenv = [\"X={{.Nope}}\"]\n"), 0o644)
	contains(t, r.fail("smol", "up"), "smol.env", "Nope")
}

func TestSmolUpPassesOnlyWhatIsConfigured(t *testing.T) {
	r := newRig(t)
	r.up()
	create := ""
	for _, c := range r.smolCalls() {
		if strings.HasPrefix(c, "machine create") {
			create = c
		}
	}
	contains(t, create, "--name sandbox", "-p ", "--mount-socket ")
	for _, flag := range []string{"--image", "--cpus", "--mem", "--storage", "--net", "--user", "--volume", "--env", "--init"} {
		if strings.Contains(create, flag) {
			t.Errorf("unconfigured create passes %s: %s", flag, create)
		}
	}
	r.ok("smol", "down")

	os.WriteFile(filepath.Join(r.home, "config.toml"),
		[]byte("[smol]\ncpus = 2\nmem = 1024\nstorage = 8\nnet = true\nnet_backend = \"gvproxy\"\n"), 0o644)
	r.up()
	calls := strings.Join(r.smolCalls(), "\n")
	contains(t, calls, "--cpus 2", "--mem 1024", "--storage 8", "--net ", "--net-backend gvproxy")
}

func TestLeaseAcquireOutput(t *testing.T) {
	r := newRig(t)
	r.up()
	out := r.ok("lease", "acquire", "shop", "console.shop.local")
	contains(t, out,
		fmt.Sprintf("export CLANKER_LEASE_APP_PORT=%d\n", r.appBase),
		fmt.Sprintf("export CLANKER_LEASE_CDP_URL=http://localhost:%d\n", r.cdpBase),
		"export CLANKER_LEASE_HOSTS='shop.local console.shop.local'")

	l := r.acquire("shop", "console.shop.local")
	if l.Slot != 0 || l.AppPort != r.appBase || l.CDPPort != r.cdpBase || l.ChromePort != r.chromeBas || l.ChromeRunning {
		t.Fatalf("lease = %+v", l)
	}
	if strings.Join(l.Hosts, " ") != "shop.local console.shop.local" || l.CDPURL != fmt.Sprintf("http://localhost:%d", r.cdpBase) {
		t.Fatalf("lease = %+v", l)
	}

	l = r.acquire("shop", "api.shop.local")
	if l.Slot != 0 || strings.Join(l.Hosts, " ") != "shop.local api.shop.local" {
		t.Fatalf("lease = %+v", l)
	}
	l = r.acquire("shop")
	if strings.Join(l.Hosts, " ") != "shop.local" {
		t.Fatalf("lease = %+v", l)
	}

	other := r.acquire("blog")
	if other.Slot != 1 || other.AppPort != r.appBase+1 {
		t.Fatalf("second lease = %+v", other)
	}
	contains(t, r.ok("lease", "list"), "shop", "blog")
	show := r.ok("lease", "show", "blog")
	contains(t, show, fmt.Sprintf("export CLANKER_LEASE_APP_PORT=%d", r.appBase+1))
	var shown lease
	if err := json.Unmarshal([]byte(r.ok("lease", "show", "--json", "blog")), &shown); err != nil || shown.Slot != 1 {
		t.Fatalf("show --json: %v %+v", err, shown)
	}
	contains(t, r.fail("lease", "show", "nope"), "unknown lease")
}

func TestLeaseValidationAndExhaustion(t *testing.T) {
	r := newRig(t)
	r.up()
	contains(t, r.fail("lease", "acquire", "Bad_Name"), "invalid lease name")
	contains(t, r.fail("lease", "acquire", ""), "invalid lease name")
	contains(t, r.fail("lease", "acquire", "ok", "host.example.com"), "must end in .local")
	contains(t, r.fail("lease", "acquire", "ok", "UP.local"), "invalid hostname")
	contains(t, r.fail("lease", "acquire", "ok", "a b.local"), "invalid hostname")
	if out := r.ok("lease", "list"); strings.Contains(out, "ok") {
		t.Fatalf("invalid acquire leaked a lease: %s", out)
	}
	r.acquire("a")
	r.acquire("b")
	r.acquire("c")
	msg := r.fail("lease", "acquire", "d")
	contains(t, msg, "no free lease", fmt.Sprintf("%d", slots))
	r.ok("lease", "release", "b")
	if l := r.acquire("d"); l.Slot != 1 {
		t.Fatalf("freed slot not reused: %+v", l)
	}
	contains(t, r.fail("lease", "acquire", "e", "a.local"), "already used by lease")
}

func TestVMNotRunning(t *testing.T) {
	r := newRig(t)
	r.up()
	os.Remove(filepath.Join(r.smolDir, "running"))
	contains(t, r.fail("lease", "acquire", "shop"), "not running")
	if out := r.ok("lease", "list"); strings.Contains(out, "shop") {
		t.Fatalf("failed acquire left a lease: %s", out)
	}
}

func TestAcquireFromVMAndHostStartRelay(t *testing.T) {
	r := newRig(t)
	r.up()
	echoServer(t, fmt.Sprintf("127.0.0.1:%d", r.chromeBas))
	echoServer(t, fmt.Sprintf("127.0.0.1:%d", r.chromeBas+1))

	execs := func() []string {
		var out []string
		for _, c := range r.smolCalls() {
			if strings.Contains(c, "machine exec") && strings.Contains(c, "relay") {
				out = append(out, c)
			}
		}
		return out
	}

	r.acquire("hostside")
	if len(execs()) != 1 {
		t.Fatalf("relay exec calls: %v", r.smolCalls())
	}
	contains(t, execs()[0], "127.0.0.1:"+fmt.Sprint(r.cdpBase), "127.0.0.2")
	eventually(t, "VM relay (host acquire)", func() error {
		return throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase))
	})
	eventually(t, "VM relay over IPv6", func() error {
		return throughput(t, fmt.Sprintf("[::1]:%d", r.cdpBase))
	})
	if err := throughput(t, fmt.Sprintf("127.0.0.2:%d", r.cdpBase)); err != nil {
		t.Fatalf("daemon relay: %v", err)
	}

	res := r.vm("lease", "acquire", "vmside")
	if res.code != 0 {
		t.Fatalf("vm acquire: %+v", res)
	}
	contains(t, res.out, fmt.Sprintf("export CLANKER_LEASE_APP_PORT=%d", r.appBase+1))
	if len(execs()) != 2 {
		t.Fatalf("relay exec calls: %v", r.smolCalls())
	}
	eventually(t, "VM relay (vm acquire)", func() error {
		return throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase+1))
	})

	pidFile := filepath.Join(r.smolDir, "guest", "relay-hostside.pid")
	b, err := os.ReadFile(pidFile)
	if err != nil {
		t.Fatal(err)
	}
	var pid int
	fmt.Sscan(string(b), &pid)
	syscall.Kill(pid, syscall.SIGKILL)
	waitFor(t, "relay death", func() bool { return throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase)) != nil })
	r.acquire("hostside")
	eventually(t, "restarted relay", func() error {
		return throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase))
	})

	r.ok("lease", "release", "hostside")
	eventually(t, "relay closed", func() error {
		if throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase)) == nil {
			return fmt.Errorf("still forwarding")
		}
		return nil
	})
	if throughput(t, fmt.Sprintf("127.0.0.2:%d", r.cdpBase)) == nil {
		t.Fatal("daemon relay still forwarding after release")
	}
}

func lanIP(t *testing.T) string {
	addrs, _ := net.InterfaceAddrs()
	for _, a := range addrs {
		if ipn, ok := a.(*net.IPNet); ok && ipn.IP.To4() != nil && !ipn.IP.IsLoopback() {
			return ipn.IP.String()
		}
	}
	t.Skip("no non-loopback IPv4 address")
	return ""
}

func TestAppForwarderOnAnnouncedAddress(t *testing.T) {
	r := newRig(t)
	ip := lanIP(t)
	r.env["CLANKERD_MDNS_SUBNETS"] = ip + "/32"
	r.up()
	echoServer(t, fmt.Sprintf("127.0.0.1:%d", r.appBase))
	r.acquire("shop")
	eventually(t, "forwarder", func() error { return throughput(t, fmt.Sprintf("%s:%d", ip, r.appBase)) })
	r.ok("lease", "release", "shop")
	eventually(t, "forwarder gone", func() error {
		if throughput(t, fmt.Sprintf("%s:%d", ip, r.appBase)) == nil {
			return fmt.Errorf("still forwarding")
		}
		return nil
	})
}

func query(t *testing.T, r *rig, name string, qtype uint16, network, addr string) *dns.Msg {
	t.Helper()
	m := new(dns.Msg)
	m.SetQuestion(dns.Fqdn(name), qtype)
	c, err := net.Dial(network, addr)
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	wire, _ := m.Pack()
	c.Write(wire)
	c.SetReadDeadline(time.Now().Add(600 * time.Millisecond))
	buf := make([]byte, 1500)
	n, err := c.Read(buf)
	if err != nil {
		return nil
	}
	resp := new(dns.Msg)
	if err := resp.Unpack(buf[:n]); err != nil {
		t.Fatal(err)
	}
	return resp
}

func TestMDNSAnswers(t *testing.T) {
	r := newRig(t)
	r.up()
	r.acquire("shop", "console.shop.local")
	v4 := fmt.Sprintf("127.0.0.1:%d", r.udp)
	v6 := fmt.Sprintf("[::1]:%d", r.udp)

	var resp *dns.Msg
	eventually(t, "A answer", func() error {
		if resp = query(t, r, "shop.local", dns.TypeA, "udp4", v4); resp == nil {
			return fmt.Errorf("no reply")
		}
		return nil
	})
	if len(resp.Answer) != 1 || resp.Answer[0].(*dns.A).A.String() != "127.0.0.1" {
		t.Fatalf("A answer: %v", resp.Answer)
	}
	resp = query(t, r, "Console.Shop.local", dns.TypeAAAA, "udp4", v4)
	if resp == nil || len(resp.Answer) != 1 || resp.Answer[0].(*dns.AAAA).AAAA.String() != "::1" {
		t.Fatalf("AAAA answer: %v", resp)
	}
	if resp = query(t, r, "shop.local", dns.TypeA, "udp6", v6); resp == nil {
		t.Fatal("no answer over IPv6")
	}
	if resp = query(t, r, "unknown.local", dns.TypeA, "udp4", v4); resp != nil {
		t.Fatalf("answered unknown name: %v", resp)
	}
	r.ok("lease", "release", "shop")
	if resp = query(t, r, "shop.local", dns.TypeA, "udp4", v4); resp != nil {
		t.Fatalf("answered released name: %v", resp)
	}
}

func TestMDNSOnlyOnListedSubnets(t *testing.T) {
	r := newRig(t)
	r.env["CLANKERD_MDNS_SUBNETS"] = "10.254.254.0/24,fd12:3456::/64"
	r.up()
	r.acquire("shop")
	if resp := query(t, r, "shop.local", dns.TypeA, "udp4", fmt.Sprintf("127.0.0.1:%d", r.udp)); resp != nil {
		t.Fatalf("answered with no local address in the subnets: %v", resp)
	}
}

func TestNoSubnetsAnnouncesNothingAndWarns(t *testing.T) {
	r := newRig(t)
	r.env["CLANKERD_MDNS_SUBNETS"] = ""
	r.up()
	res := r.run("lease", "acquire", "shop")
	if res.code != 0 {
		t.Fatalf("%+v", res)
	}
	contains(t, res.err, "warning", "nothing announced")
	if resp := query(t, r, "shop.local", dns.TypeA, "udp4", fmt.Sprintf("127.0.0.1:%d", r.udp)); resp != nil {
		t.Fatalf("announced without subnets: %v", resp)
	}
	r.env["CLANKERD_MDNS_SUBNETS"] = "bogus"
	contains(t, r.fail("smol", "up"), "invalid subnet")
	r.env["CLANKERD_MDNS_SUBNETS"] = ""
}

func TestBrowser(t *testing.T) {
	r := newRig(t)
	r.up()
	contains(t, r.fail("browser", "start", "shop"), "unknown lease")
	if out := r.ok("lease", "list"); strings.Contains(out, "shop") {
		t.Fatalf("browser start allocated a lease: %s", out)
	}
	l := r.acquire("shop")
	r.ok("browser", "start", "shop")
	waitFor(t, "chrome", func() bool { return len(r.chromeStarts()) == 1 })
	line := r.chromeStarts()[0]
	profile := filepath.Join(r.home, "state", "sandbox", "profiles", "shop")
	contains(t, line, "--user-data-dir="+profile, fmt.Sprintf("--remote-debugging-port=%d", l.ChromePort))
	if strings.Contains(line, "http") {
		t.Fatalf("chrome was given a URL: %s", line)
	}
	var pid int
	fmt.Sscanf(line, "pid=%d", &pid)
	if !alive(pid) {
		t.Fatal("chrome not running")
	}
	if !r.acquireShow("shop").ChromeRunning {
		t.Fatal("lease show does not report chrome")
	}

	r.ok("browser", "start", "shop")
	time.Sleep(200 * time.Millisecond)
	if n := len(r.chromeStarts()); n != 1 {
		t.Fatalf("chrome started %d times", n)
	}

	r.ok("browser", "stop", "shop")
	waitFor(t, "chrome exit", func() bool { return !alive(pid) })
	r.ok("browser", "stop", "shop")

	os.MkdirAll(profile, 0o755)
	os.WriteFile(filepath.Join(profile, "Cookies"), []byte("x"), 0o644)
	r.ok("lease", "release", "shop")
	if _, err := os.Stat(filepath.Join(profile, "Cookies")); err != nil {
		t.Fatalf("profile lost on release: %v", err)
	}
	r.acquire("shop")
	r.ok("lease", "release", "shop", "--purge")
	if _, err := os.Stat(profile); !os.IsNotExist(err) {
		t.Fatalf("profile survived --purge: %v", err)
	}
}

func (r *rig) acquireShow(name string) lease {
	var l lease
	if err := json.Unmarshal([]byte(r.ok("lease", "show", "--json", name)), &l); err != nil {
		r.t.Fatal(err)
	}
	return l
}

func TestReleaseStopsChrome(t *testing.T) {
	r := newRig(t)
	r.up()
	r.acquire("shop")
	r.ok("browser", "start", "shop")
	waitFor(t, "chrome", func() bool { return len(r.chromeStarts()) == 1 })
	var pid int
	fmt.Sscanf(r.chromeStarts()[0], "pid=%d", &pid)
	r.ok("lease", "release", "shop")
	waitFor(t, "chrome exit", func() bool { return !alive(pid) })
}

func TestStateSurvivesDaemonRestart(t *testing.T) {
	r := newRig(t)
	r.up()
	echoServer(t, fmt.Sprintf("127.0.0.1:%d", r.chromeBas+1))
	r.acquire("shop", "console.shop.local")
	r.acquire("blog")
	r.ok("browser", "start", "blog")
	waitFor(t, "chrome", func() bool { return len(r.chromeStarts()) == 1 })

	pid := r.daemonPID()
	syscall.Kill(pid, syscall.SIGTERM)
	waitFor(t, "daemon exit", func() bool { return !alive(pid) })
	contains(t, r.fail("lease", "list"), "not running")

	r.up()
	out := r.ok("lease", "list")
	contains(t, out, "shop", "blog", "console.shop.local")
	if l := r.acquireShow("blog"); l.Slot != 1 || !l.ChromeRunning {
		t.Fatalf("blog after restart: %+v", l)
	}
	if resp := query(t, r, "console.shop.local", dns.TypeA, "udp4", fmt.Sprintf("127.0.0.1:%d", r.udp)); resp == nil {
		t.Fatal("names not restored")
	}
	eventually(t, "cdp relay restored", func() error {
		return throughput(t, fmt.Sprintf("127.0.0.2:%d", r.cdpBase+1))
	})
	r.ok("browser", "start", "blog")
	time.Sleep(200 * time.Millisecond)
	if n := len(r.chromeStarts()); n != 1 {
		t.Fatalf("running chrome was not re-adopted: %d starts", n)
	}
	if l := r.acquire("third"); l.Slot != 2 {
		t.Fatalf("slot after restart: %+v", l)
	}
}

func TestSmolUpRecreatesRelaysAfterVMRestart(t *testing.T) {
	r := newRig(t)
	r.up()
	echoServer(t, fmt.Sprintf("127.0.0.1:%d", r.chromeBas))
	r.acquire("shop")
	eventually(t, "relay", func() error { return throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase)) })
	b, _ := os.ReadFile(filepath.Join(r.smolDir, "guest", "relay-shop.pid"))
	var pid int
	fmt.Sscan(string(b), &pid)
	syscall.Kill(pid, syscall.SIGKILL)
	os.RemoveAll(filepath.Join(r.smolDir, "guest"))
	waitFor(t, "relay death", func() bool { return throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase)) != nil })
	os.Remove(filepath.Join(r.smolDir, "running"))
	r.up()
	eventually(t, "relay recreated", func() error { return throughput(t, fmt.Sprintf("127.0.0.1:%d", r.cdpBase)) })
}

func TestControlSocket(t *testing.T) {
	r := newRig(t)
	r.up()
	fi, err := os.Stat(r.sock())
	if err != nil {
		t.Fatal(err)
	}
	if fi.Mode().Perm() != 0o600 {
		t.Fatalf("socket mode %v", fi.Mode().Perm())
	}
	call := func(req string) string {
		c, err := net.Dial("unix", r.sock())
		if err != nil {
			t.Fatal(err)
		}
		defer c.Close()
		fmt.Fprintln(c, req)
		c.SetReadDeadline(time.Now().Add(2 * time.Second))
		buf := make([]byte, 4096)
		n, _ := c.Read(buf)
		return string(buf[:n])
	}
	contains(t, call(`{"v":999,"op":"lease.list"}`), `"ok":false`, "protocol version")
	contains(t, call(`{"v":1,"op":"exec","cmd":"id"}`), `"ok":false`, "unknown operation")
	contains(t, call(`{"v":1,"op":"lease.acquire","name":"x","hosts":["a.local; id"]}`), `"ok":false`, "invalid hostname")
	contains(t, call(`not json`), `"ok":false`)
	contains(t, call(`{"v":1,"op":"lease.list"}`), `"ok":true`)
}

func TestConfigLayering(t *testing.T) {
	r := newRig(t)
	delete(r.env, "CLANKERD_SLOTS")
	os.WriteFile(filepath.Join(r.home, "config.toml"), []byte("[ports]\nslots = 1\n"), 0o644)
	r.up()
	r.acquire("a")
	contains(t, r.fail("lease", "acquire", "b"), "no free lease")
	r.ok("smol", "down")

	r.env["CLANKERD_SLOTS"] = "2"
	r.up()
	r.acquire("a")
	r.acquire("b")
	contains(t, r.fail("lease", "acquire", "c"), "no free lease")
	r.ok("smol", "down")

	r.up2("--slots", "3")
	r.acquire("a")
	r.acquire("b")
	r.acquire("c")
}

func (r *rig) up2(flags ...string) { r.ok(append([]string{"smol", "up"}, flags...)...) }

func TestProjectConfigFoundByWalkingUp(t *testing.T) {
	r := newRig(t)
	delete(r.env, "CLANKERD_HOME")
	delete(r.env, "CLANKERD_SLOTS")
	proj := filepath.Join(r.work, "proj")
	deep := filepath.Join(proj, "a", "b")
	os.MkdirAll(filepath.Join(proj, ".clankerd"), 0o755)
	os.MkdirAll(deep, 0o755)
	os.WriteFile(filepath.Join(proj, ".clankerd", "config.toml"), []byte("[vm]\nname = \"projvm\"\n[ports]\nslots = 1\n"), 0o644)
	r.work = deep
	r.ok("smol", "up")
	r.ok("lease", "acquire", "a")
	contains(t, r.fail("lease", "acquire", "b"), "no free lease")
	contains(t, strings.Join(r.smolCalls(), "\n"), "--name projvm")
}

func TestVersion(t *testing.T) {
	r := newRig(t)
	contains(t, r.ok("version"), "clankerctl")
}

func TestSmolUpRefusesAVMFromAnotherConfiguration(t *testing.T) {
	r := newRig(t)
	r.up()
	os.WriteFile(filepath.Join(r.home, "config.toml"), []byte("[smol]\ncpus = 2\n"), 0o644)
	contains(t, r.fail("smol", "up"), "not created from the current configuration")
	r.ok("smol", "down")
	r.up()
	contains(t, strings.Join(r.smolCalls(), "\n"), "--cpus 2")

	r.ok("smol", "down")
	os.WriteFile(filepath.Join(r.smolDir, "exists"), nil, 0o644)
	contains(t, r.fail("smol", "up"), "not created from the current configuration")
}

func TestSandboxDetection(t *testing.T) {
	r := newRig(t)
	r.env["CLANKERD_CMDLINE_PATH"] = r.fakeSmolvmCmdline()
	r.env["CLANKERD_GUEST_SOCKET"] = filepath.Join(r.work, "absent.sock")
	contains(t, r.fail("lease", "acquire", "shop"), "running in a smolmachine", "control socket", "is missing")
	contains(t, r.fail("smol", "up"), "running in a smolmachine", "is missing")

	r.env["CLANKERD_CMDLINE_PATH"] = filepath.Join(r.work, "no-cmdline")
	dockerenv := filepath.Join(r.work, "dockerenv")
	os.WriteFile(dockerenv, nil, 0o644)
	r.env["CLANKERD_DOCKERENV_PATH"] = dockerenv
	contains(t, r.fail("lease", "acquire", "shop"), "running in a docker", "is missing")

	os.Remove(dockerenv)
	delete(r.env, "CLANKERD_GUEST_SOCKET")
	contains(t, r.fail("lease", "acquire", "shop"), "clankerd is not running", "smol up")
}

func TestSmolRefusesInsideTheVM(t *testing.T) {
	r := newRig(t)
	r.up()
	contains(t, r.vm("smol", "status").err, "run on the host, not in a smolmachine")
}

func TestStalePIDOfAnotherProcessIsNotKilled(t *testing.T) {
	r := newRig(t)
	r.up()
	r.acquire("shop")
	r.ok("browser", "start", "shop")
	waitFor(t, "chrome", func() bool { return len(r.chromeStarts()) == 1 })
	var chromePID int
	fmt.Sscanf(r.chromeStarts()[0], "pid=%d", &chromePID)

	bystander := exec.Command("sleep", "600")
	if err := bystander.Start(); err != nil {
		t.Fatal(err)
	}
	defer bystander.Process.Kill()
	go bystander.Wait()

	daemon := r.daemonPID()
	syscall.Kill(daemon, syscall.SIGTERM)
	waitFor(t, "daemon exit", func() bool { return !alive(daemon) })
	syscall.Kill(chromePID, syscall.SIGTERM)
	waitFor(t, "chrome exit", func() bool { return !alive(chromePID) })

	stateFile := filepath.Join(r.home, "state", "sandbox", "state.json")
	b, _ := os.ReadFile(stateFile)
	b = []byte(strings.Replace(string(b), fmt.Sprintf(`"chrome_pid": %d`, chromePID), fmt.Sprintf(`"chrome_pid": %d`, bystander.Process.Pid), 1))
	os.WriteFile(stateFile, b, 0o600)

	r.up()
	if r.acquireShow("shop").ChromeRunning {
		t.Fatal("an unrelated process was adopted as Chrome")
	}
	r.ok("lease", "release", "shop")
	if !alive(bystander.Process.Pid) {
		t.Fatal("release killed an unrelated process")
	}
}

func TestOperationsRunConcurrently(t *testing.T) {
	r := newRig(t)
	r.env["FAKE_SMOLVM_EXEC_DELAY"] = "3"
	r.up()

	type out struct {
		l   lease
		err string
	}
	acquire := func(name string, ch chan<- out) {
		res := r.run("lease", "acquire", "--json", name)
		var l lease
		json.Unmarshal([]byte(res.out), &l)
		ch <- out{l, res.err}
	}

	start := time.Now()
	ch := make(chan out, 4)
	go acquire("a", ch)
	go acquire("b", ch)
	go acquire("c", ch)
	time.Sleep(500 * time.Millisecond)

	listStart := time.Now()
	r.ok("lease", "list")
	if d := time.Since(listStart); d > 2*time.Second {
		t.Fatalf("lease list took %v while acquires were running", d)
	}

	slotsSeen := map[int]bool{}
	for i := 0; i < 3; i++ {
		o := <-ch
		if o.err != "" {
			t.Fatalf("acquire failed: %s", o.err)
		}
		slotsSeen[o.l.Slot] = true
	}
	if len(slotsSeen) != 3 {
		t.Fatalf("concurrent acquires shared slots: %v", slotsSeen)
	}
	if d := time.Since(start); d > 6*time.Second {
		t.Fatalf("three acquires of 3s each took %v; they ran one after another", d)
	}

	go acquire("a", ch)
	go acquire("a", ch)
	first, second := <-ch, <-ch
	if first.err != "" || second.err != "" || first.l.Slot != second.l.Slot || first.l.Name != "a" {
		t.Fatalf("same-name acquires: %+v %+v", first, second)
	}

	rel := make(chan result, 1)
	go func() { rel <- r.run("lease", "release", "b") }()
	go acquire("b", ch)
	if res := <-rel; res.code != 0 {
		t.Fatalf("release: %+v", res)
	}
	if o := <-ch; o.err != "" {
		t.Fatalf("re-acquire: %s", o.err)
	}
	r.acquireShow("b")
}

func TestConcurrentSmolUpAndDown(t *testing.T) {
	r := newRig(t)
	results := make(chan result, 8)
	for i := 0; i < 8; i++ {
		go func() { results <- r.run("smol", "up") }()
	}
	for i := 0; i < 8; i++ {
		if res := <-results; res.code != 0 {
			t.Fatalf("smol up failed: %+v", res)
		}
	}
	creates := 0
	for _, c := range r.smolCalls() {
		if strings.HasPrefix(c, "machine create") {
			creates++
		}
	}
	if creates != 1 {
		t.Fatalf("machine create ran %d times", creates)
	}
	if !alive(r.daemonPID()) {
		t.Fatal("no daemon")
	}

	for i := 0; i < 4; i++ {
		go func() { results <- r.run("smol", "down") }()
	}
	for i := 0; i < 4; i++ {
		if res := <-results; res.code != 0 {
			t.Fatalf("smol down failed: %+v", res)
		}
	}
	contains(t, r.ok("smol", "status"), "daemon: stopped", "vm: missing")
}
