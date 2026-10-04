// Package e2e drives the real clankerd and clankerctl binaries. Only what is outside our code is
// faked: smolvm and Chrome are small shell scripts on PATH, and the mDNS group is a loopback address.
package e2e

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math/rand"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

var binDir string

const fakeSmolvm = `#!/bin/sh
printf '%s\n' "$(echo "$*" | tr '\n' ' ')" >> "$FAKE_SMOLVM_DIR/calls.log"
[ "$1" = machine ] || exit 2
sub=$2; shift 2
case $sub in
  create) touch "$FAKE_SMOLVM_DIR/exists";;
  start) [ -f "$FAKE_SMOLVM_DIR/exists" ] || exit 1; touch "$FAKE_SMOLVM_DIR/running";;
  stop) rm -f "$FAKE_SMOLVM_DIR/running";;
  delete) rm -f "$FAKE_SMOLVM_DIR/exists" "$FAKE_SMOLVM_DIR/running";;
  status)
    [ -f "$FAKE_SMOLVM_DIR/exists" ] || { echo "machine not found" >&2; exit 1; }
    if [ -f "$FAKE_SMOLVM_DIR/running" ]; then echo running; else echo stopped; fi;;
  exec)
    [ -f "$FAKE_SMOLVM_DIR/running" ] || { echo "machine not running" >&2; exit 1; }
    shift 3 # --name VM --
    exec "$@";;
esac
`

const fakeChrome = `#!/bin/sh
echo "pid=$$ $*" >> "$FAKE_CHROME_LOG"
exec sleep 600
`

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "clankerd-e2e-bin")
	if err != nil {
		panic(err)
	}
	binDir = dir
	for _, p := range []string{"clankerd", "clankerctl"} {
		out, err := exec.Command("go", "build", "-o", filepath.Join(dir, p), "../../cmd/"+p).CombinedOutput()
		if err != nil {
			fmt.Fprintf(os.Stderr, "build %s: %v\n%s", p, err, out)
			os.Exit(1)
		}
	}
	os.WriteFile(filepath.Join(dir, "smolvm"), []byte(fakeSmolvm), 0o755)
	os.WriteFile(filepath.Join(dir, "fake-chrome"), []byte(fakeChrome), 0o755)
	code := m.Run()
	os.RemoveAll(dir)
	os.Exit(code)
}

type rig struct {
	t         *testing.T
	home      string
	smolDir   string
	chromeLog string
	work      string
	env       map[string]string
	appBase   int
	cdpBase   int
	chromeBas int
	udp       int
}

func freeTCPRange(n int, hosts ...string) int {
	for {
		base := 20000 + rand.Intn(30000)
		ok := true
		for i := 0; i < n && ok; i++ {
			for _, h := range hosts {
				l, err := net.Listen("tcp", net.JoinHostPort(h, strconv.Itoa(base+i)))
				if err != nil {
					ok = false
					break
				}
				l.Close()
			}
		}
		if ok {
			return base
		}
	}
}

func freeUDP() int {
	c, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		panic(err)
	}
	defer c.Close()
	return c.LocalAddr().(*net.UDPAddr).Port
}

const slots = 3

func newRig(t *testing.T) *rig {
	t.Helper()
	r := &rig{t: t}
	r.home = t.TempDir()
	r.smolDir = t.TempDir()
	r.work = t.TempDir()
	r.chromeLog = filepath.Join(r.smolDir, "chrome.log")
	r.appBase = freeTCPRange(slots, "127.0.0.1", "0.0.0.0")
	r.cdpBase = freeTCPRange(slots, "127.0.0.1", "127.0.0.2", "::1")
	r.chromeBas = freeTCPRange(slots, "127.0.0.1")
	r.udp = freeUDP()
	r.env = map[string]string{
		"PATH":                      binDir + ":" + os.Getenv("PATH"),
		"HOME":                      r.work,
		"CLANKERD_HOME":             r.home,
		"CLANKERD_SLOTS":            strconv.Itoa(slots),
		"CLANKERD_APP_PORT_BASE":    strconv.Itoa(r.appBase),
		"CLANKERD_CDP_PORT_BASE":    strconv.Itoa(r.cdpBase),
		"CLANKERD_CHROME_PORT_BASE": strconv.Itoa(r.chromeBas),
		"CLANKERD_RELAY_BIND":       "127.0.0.2",
		"CLANKERD_HOST_ADDR":        "127.0.0.2",
		"CLANKERD_MDNS_SUBNETS":     "127.0.0.0/8,::1/128",
		"CLANKERD_MDNS_GROUP4":      fmt.Sprintf("127.0.0.1:%d", r.udp),
		"CLANKERD_MDNS_GROUP6":      fmt.Sprintf("[::1]:%d", r.udp),
		"CLANKERD_CHROME_BIN":       filepath.Join(binDir, "fake-chrome"),
		"CLANKERD_GUEST_BIN":        "",
		"CLANKERD_GUEST_DIR":        filepath.Join(r.smolDir, "guest"),
		"FAKE_SMOLVM_DIR":           r.smolDir,
		"FAKE_CHROME_LOG":           r.chromeLog,
	}
	t.Cleanup(func() { r.run("smol", "down") })
	return r
}

func (r *rig) environ() []string {
	var e []string
	for k, v := range r.env {
		e = append(e, k+"="+v)
	}
	return e
}

type result struct {
	out, err string
	code     int
}

func (r *rig) run(args ...string) result {
	r.t.Helper()
	cmd := exec.Command(filepath.Join(binDir, "clankerctl"), args...)
	cmd.Dir = r.work
	cmd.Env = r.environ()
	var so, se bytes.Buffer
	cmd.Stdout, cmd.Stderr = &so, &se
	done := make(chan error, 1)
	if err := cmd.Start(); err != nil {
		r.t.Fatal(err)
	}
	go func() { done <- cmd.Wait() }()
	select {
	case err := <-done:
		code := 0
		if ee, ok := err.(*exec.ExitError); ok {
			code = ee.ExitCode()
		} else if err != nil {
			r.t.Fatal(err)
		}
		return result{so.String(), se.String(), code}
	case <-time.After(30 * time.Second):
		cmd.Process.Kill()
		r.t.Fatalf("clankerctl %v timed out", args)
		return result{}
	}
}

func (r *rig) ok(args ...string) string {
	r.t.Helper()
	res := r.run(args...)
	if res.code != 0 {
		r.t.Fatalf("clankerctl %v: exit %d\nstdout: %s\nstderr: %s", args, res.code, res.out, res.err)
	}
	return res.out
}

func (r *rig) fail(args ...string) string {
	r.t.Helper()
	res := r.run(args...)
	if res.code == 0 {
		r.t.Fatalf("clankerctl %v: expected failure, got\n%s", args, res.out)
	}
	return res.err
}

func (r *rig) vm(args ...string) result {
	r.t.Helper()
	saved := r.env["CLANKERD_GUEST_SOCKET"]
	r.env["CLANKERD_GUEST_SOCKET"] = r.sock()
	defer func() { r.env["CLANKERD_GUEST_SOCKET"] = saved }()
	for _, k := range []string{"CLANKERD_HOME", "CLANKERD_SLOTS"} {
		defer func(k, v string) { r.env[k] = v }(k, r.env[k])
		delete(r.env, k)
	}
	return r.run(args...)
}

func (r *rig) sock() string { return filepath.Join(r.home, "run", "sandbox.sock") }

func (r *rig) up() {
	r.t.Helper()
	r.ok("smol", "up")
}

func (r *rig) smolCalls() []string {
	b, _ := os.ReadFile(filepath.Join(r.smolDir, "calls.log"))
	var out []string
	for _, l := range strings.Split(strings.TrimSpace(string(b)), "\n") {
		if l != "" {
			out = append(out, l)
		}
	}
	return out
}

func (r *rig) chromeStarts() []string {
	b, _ := os.ReadFile(r.chromeLog)
	var out []string
	for _, l := range strings.Split(strings.TrimSpace(string(b)), "\n") {
		if l != "" {
			out = append(out, l)
		}
	}
	return out
}

type lease struct {
	Name          string   `json:"name"`
	Slot          int      `json:"slot"`
	AppPort       int      `json:"app_port"`
	CDPPort       int      `json:"cdp_port"`
	ChromePort    int      `json:"chrome_port"`
	Hosts         []string `json:"hosts"`
	CDPURL        string   `json:"cdp_url"`
	ChromeRunning bool     `json:"chrome_running"`
}

func (r *rig) acquire(args ...string) lease {
	r.t.Helper()
	out := r.ok(append([]string{"lease", "acquire", "--json"}, args...)...)
	var l lease
	if err := json.Unmarshal([]byte(out), &l); err != nil {
		r.t.Fatalf("bad json %q: %v", out, err)
	}
	return l
}

func (r *rig) daemonPID() int {
	b, err := os.ReadFile(filepath.Join(r.home, "run", "sandbox.pid"))
	if err != nil {
		return 0
	}
	n, _ := strconv.Atoi(strings.TrimSpace(string(b)))
	return n
}

func alive(pid int) bool { return pid > 0 && syscall.Kill(pid, 0) == nil }

func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	for i := 0; i < 100; i++ {
		if cond() {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

func echoServer(t *testing.T, addr string) {
	t.Helper()
	l, err := net.Listen("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { l.Close() })
	go func() {
		for {
			c, err := l.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				buf := make([]byte, 64)
				n, _ := c.Read(buf)
				c.Write(buf[:n])
			}()
		}
	}()
}

func throughput(t *testing.T, addr string) error {
	c, err := net.DialTimeout("tcp", addr, 2*time.Second)
	if err != nil {
		return err
	}
	defer c.Close()
	c.SetDeadline(time.Now().Add(2 * time.Second))
	if _, err := c.Write([]byte("ping")); err != nil {
		return err
	}
	buf := make([]byte, 4)
	if _, err := c.Read(buf); err != nil {
		return err
	}
	if string(buf) != "ping" {
		return fmt.Errorf("got %q", buf)
	}
	return nil
}

func eventually(t *testing.T, what string, f func() error) {
	t.Helper()
	var err error
	for i := 0; i < 60; i++ {
		if err = f(); err == nil {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("%s: %v", what, err)
}
