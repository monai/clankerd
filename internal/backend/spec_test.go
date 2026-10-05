package backend

import (
	"reflect"
	"strings"
	"testing"
)

func base() CreateSpec {
	return CreateSpec{
		Image: "img", CPUs: 2, Mem: 1024, Storage: 8, User: "u",
		Volumes: []string{"/a:/a", "/b:/b:ro"}, Env: []string{"A=1", "B=2"},
		Init: []string{"i"}, Cmd: []string{"c"}, PortFrom: 4000, PortTo: 4009,
		Socket: "/s", GuestSock: "/g",
	}
}

func TestUpdateArgs(t *testing.T) {
	for name, tc := range map[string]struct {
		edit func(*CreateSpec)
		want string
	}{
		"unchanged":      {func(*CreateSpec) {}, ""},
		"add volume":     {func(c *CreateSpec) { c.Volumes = append(c.Volumes, "/c:/c") }, "-v /c:/c"},
		"remove volume":  {func(c *CreateSpec) { c.Volumes = c.Volumes[:1] }, "--remove-volume /b:/b"},
		"mode change":    {func(c *CreateSpec) { c.Volumes[1] = "/b:/b" }, "--remove-volume /b:/b -v /b:/b"},
		"all volumes":    {func(c *CreateSpec) { c.Volumes = nil }, "--remove-volume /a:/a --remove-volume /b:/b"},
		"ports":          {func(c *CreateSpec) { c.PortFrom, c.PortTo = 5000, 5004 }, "--remove-port 4000-4009:4000-4009 -p 5000-5004:5000-5004"},
		"cpus and mem":   {func(c *CreateSpec) { c.CPUs, c.Mem = 4, 2048 }, "--cpus 4 --mem 2048"},
		"storage grows":  {func(c *CreateSpec) { c.Storage = 16 }, "--storage 16"},
		"storage shrink": {func(c *CreateSpec) { c.Storage = 4 }, "--storage 4"},
		"unset keeps":    {func(c *CreateSpec) { c.CPUs, c.Mem, c.Storage = 0, 0, 0 }, ""},
		"net on":         {func(c *CreateSpec) { c.Net = true }, "--net"},
		"env":            {func(c *CreateSpec) { c.Env = []string{"A=9", "C=3"} }, "--remove-env B -e A=9 -e C=3"},
	} {
		prev, next := base(), base()
		tc.edit(&next)
		got := strings.Join(UpdateArgs(prev, Merge(prev, next)), " ")
		if got != tc.want {
			t.Errorf("%s: got %q, want %q", name, got, tc.want)
		}
	}
	prev, next := base(), base()
	prev.Net = true
	if got := strings.Join(UpdateArgs(prev, next), " "); got != "--no-net" {
		t.Errorf("net off: %q", got)
	}
}

func TestMergeKeepsUnsetResources(t *testing.T) {
	prev, next := base(), base()
	next.CPUs, next.Mem, next.Storage = 0, 0, 0
	if m := Merge(prev, next); m.CPUs != 2 || m.Mem != 1024 || m.Storage != 8 {
		t.Fatalf("%+v", m)
	}
}

func TestFixedFields(t *testing.T) {
	prev := base()
	next := base()
	next.Image, next.User, next.Init, next.Cmd, next.NetBackend, next.Socket = "x", "x", nil, nil, "x", "/x"
	got := prev.Fixed(next)
	if want := []string{"image", "user", "init", "cmd", "net_backend", "socket"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("%v", got)
	}
	next = base()
	next.Volumes, next.Env, next.CPUs = nil, nil, 9
	if got := prev.Fixed(next); len(got) != 0 {
		t.Fatalf("%v", got)
	}
}

func TestParseSpecRoundTripAndLegacyHash(t *testing.T) {
	b := base().Marshal()
	got, ok := ParseSpec(b)
	if !ok || !reflect.DeepEqual(got, base()) {
		t.Fatalf("%v %+v", ok, got)
	}
	if _, ok := ParseSpec([]byte("null")); ok {
		t.Fatal("null parsed as spec")
	}
	if _, ok := ParseSpec([]byte("3f2a9c")); ok {
		t.Fatal("hash parsed as spec")
	}
}
