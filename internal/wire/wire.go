// Package wire is the control protocol: one JSON request and one JSON reply per unix-socket
// connection, every message carrying the protocol version.
package wire

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"regexp"
	"strings"
	"time"
)

const Version = 1

// GuestSocket is where smolvm mounts the daemon's control socket inside the VM.
const GuestSocket = "/run/clankerd/ctl.sock"

const (
	OpLeaseAcquire = "lease.acquire"
	OpLeaseRelease = "lease.release"
	OpLeaseList    = "lease.list"
	OpLeaseShow    = "lease.show"
	OpBrowserStart = "browser.start"
	OpBrowserStop  = "browser.stop"
	OpResync       = "resync"
)

type Request struct {
	V     int      `json:"v"`
	Op    string   `json:"op"`
	Name  string   `json:"name,omitempty"`
	Hosts []string `json:"hosts,omitempty"`
	Purge bool     `json:"purge,omitempty"`
}

type Lease struct {
	Name          string   `json:"name"`
	Slot          int      `json:"slot"`
	AppPort       int      `json:"app_port"`
	CDPPort       int      `json:"cdp_port"`
	ChromePort    int      `json:"chrome_port"`
	Hosts         []string `json:"hosts"`
	CDPURL        string   `json:"cdp_url"`
	ChromeRunning bool     `json:"chrome_running"`
}

type Response struct {
	V        int      `json:"v"`
	OK       bool     `json:"ok"`
	Error    string   `json:"error,omitempty"`
	Lease    *Lease   `json:"lease,omitempty"`
	Leases   []Lease  `json:"leases,omitempty"`
	Warnings []string `json:"warnings,omitempty"`
}

var (
	nameRE = regexp.MustCompile(`^[a-z0-9-]{1,40}$`)
	hostRE = regexp.MustCompile(`^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$`)
)

func ValidateName(n string) error {
	if !nameRE.MatchString(n) {
		return fmt.Errorf("invalid lease name %q (use 1-40 of a-z 0-9 -)", n)
	}
	return nil
}

func ValidateHost(h string) error {
	if !hostRE.MatchString(h) || len(h) > 253 {
		return fmt.Errorf("invalid hostname %q (use a-z 0-9 . - only)", h)
	}
	if !strings.HasSuffix(h, ".local") || h == ".local" {
		return fmt.Errorf("hostname %q must end in .local", h)
	}
	return nil
}

const maxMessage = 64 << 10

// ReadRequest reads one request line.
func ReadRequest(r io.Reader) (*Request, error) {
	line, err := bufio.NewReaderSize(io.LimitReader(r, maxMessage), 4096).ReadBytes('\n')
	if err != nil && !(errors.Is(err, io.EOF) && len(line) > 0) {
		return nil, fmt.Errorf("reading request: %w", err)
	}
	var req Request
	if err := json.Unmarshal(line, &req); err != nil {
		return nil, fmt.Errorf("malformed request: %w", err)
	}
	return &req, nil
}

func WriteResponse(w io.Writer, resp *Response) error {
	resp.V = Version
	b, err := json.Marshal(resp)
	if err != nil {
		return err
	}
	_, err = w.Write(append(b, '\n'))
	return err
}

// ErrDaemonDown means nothing is listening on the control socket.
var ErrDaemonDown = errors.New("clankerd is not running")

// Call sends one request to the daemon and returns its reply. A reply with ok=false becomes an error.
func Call(sock string, req Request) (*Response, error) {
	req.V = Version
	c, err := net.DialTimeout("unix", sock, 3*time.Second)
	if err != nil {
		return nil, fmt.Errorf("%w (cannot connect to %s: %v)", ErrDaemonDown, sock, err)
	}
	defer c.Close()
	c.SetDeadline(time.Now().Add(60 * time.Second))
	b, _ := json.Marshal(req)
	if _, err := c.Write(append(b, '\n')); err != nil {
		return nil, err
	}
	line, err := bufio.NewReader(io.LimitReader(c, 4<<20)).ReadBytes('\n')
	if err != nil && len(line) == 0 {
		return nil, fmt.Errorf("reading reply: %w", err)
	}
	var resp Response
	if err := json.Unmarshal(line, &resp); err != nil {
		return nil, fmt.Errorf("malformed reply: %w", err)
	}
	if resp.V != Version {
		return nil, fmt.Errorf("protocol version mismatch: clankerctl speaks %d, clankerd speaks %d; "+
			"run the same build on host and VM (`clankerctl smol up` relinks the VM's copy)", Version, resp.V)
	}
	if !resp.OK {
		return &resp, errors.New(resp.Error)
	}
	return &resp, nil
}
