package config

import (
	"flag"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func load(t *testing.T, toml string, env map[string]string, args ...string) (*Config, *Flags) {
	t.Helper()
	home := t.TempDir()
	t.Setenv("CLANKERD_HOME", home)
	t.Setenv("CLANKERD_VM", "t")
	if toml != "" {
		if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(toml), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	for k, v := range env {
		t.Setenv(k, v)
	}
	fs := flag.NewFlagSet("t", flag.ContinueOnError)
	f := AddFlags(fs)
	if err := fs.Parse(args); err != nil {
		t.Fatal(err)
	}
	c, err := Load(f, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	return c, f
}

func TestSmolKeysLayerFlagsOverEnvOverFile(t *testing.T) {
	c, _ := load(t, "[smol]\nimage = \"file\"\ncpus = 1\nvolumes = [\"/a:/a\"]\n", nil)
	if c.Smol.Image != "file" || c.Smol.CPUs != 1 || !reflect.DeepEqual(c.Smol.Volumes, []string{"/a:/a"}) {
		t.Fatalf("file: %+v", c.Smol)
	}
	env := map[string]string{"CLANKERD_SMOL_IMAGE": "env", "CLANKERD_SMOL_VOLUMES": `["/b:/b","/c:/c:ro"]`, "CLANKERD_SMOL_NET": "true"}
	c, _ = load(t, "[smol]\nimage = \"file\"\ncpus = 1\nvolumes = [\"/a:/a\"]\n", env)
	if c.Smol.Image != "env" || c.Smol.CPUs != 1 || !c.Smol.Net ||
		!reflect.DeepEqual(c.Smol.Volumes, []string{"/b:/b", "/c:/c:ro"}) {
		t.Fatalf("env: %+v", c.Smol)
	}
	c, _ = load(t, "", env, "--smol-image=flag", "--smol-volumes=/d:/d", "--smol-volumes=/e:/e", "--smol-net=false",
		"--smol-net-backend=gvproxy", "--smol-cmd=sh", "--smol-cmd=-c", "--smol-env=A=1,2")
	want := Smol{Image: "flag", Volumes: []string{"/d:/d", "/e:/e"}, NetBackend: "gvproxy", Cmd: []string{"sh", "-c"}, Env: []string{"A=1,2"}}
	if !reflect.DeepEqual(c.Smol, want) {
		t.Fatalf("flags: %+v", c.Smol)
	}
}

func TestBareSmolNetFlag(t *testing.T) {
	if c, _ := load(t, "", nil, "--smol-net"); !c.Smol.Net {
		t.Fatal("--smol-net did not enable net")
	}
}

func TestCommaListsAcceptEveryForm(t *testing.T) {
	for name, tc := range map[string]struct {
		env  map[string]string
		args []string
	}{
		"comma env":     {map[string]string{"CLANKERD_RELAY_BIND": "10.0.0.1, 10.0.0.2"}, nil},
		"json env":      {map[string]string{"CLANKERD_RELAY_BIND": `["10.0.0.1","10.0.0.2"]`}, nil},
		"repeated flag": {nil, []string{"--relay-bind=10.0.0.1", "--relay-bind=10.0.0.2"}},
		"comma flag":    {nil, []string{"--relay-bind=10.0.0.1,10.0.0.2"}},
	} {
		c, _ := load(t, "", tc.env, tc.args...)
		if !reflect.DeepEqual(c.RelayBind, []string{"10.0.0.1", "10.0.0.2"}) {
			t.Errorf("%s: %v", name, c.RelayBind)
		}
	}
	c, _ := load(t, "", map[string]string{"CLANKERD_MDNS_SUBNETS": `["192.168.1.0/24"]`})
	if len(c.MDNSSubnets) != 1 {
		t.Errorf("subnets: %v", c.MDNSSubnets)
	}
}

func TestArgsReemitsRepeatedFlags(t *testing.T) {
	_, f := load(t, "", nil, "--smol-volumes=/d:/d", "--smol-volumes=/e:/e", "--smol-net", "--vm=x")
	got := f.Args()
	want := []string{"--smol-net=true", "--smol-volumes=/d:/d", "--smol-volumes=/e:/e", "--vm=x"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("%v", got)
	}
	fs := flag.NewFlagSet("t", flag.ContinueOnError)
	f2 := AddFlags(fs)
	if err := fs.Parse(got); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(f2.Args(), want) {
		t.Fatalf("round trip: %v", f2.Args())
	}
}

func TestBadJSONListIsAnError(t *testing.T) {
	t.Setenv("CLANKERD_HOME", t.TempDir())
	t.Setenv("CLANKERD_VM", "t")
	t.Setenv("CLANKERD_SMOL_VOLUMES", `["/a:/a"`)
	if _, err := Load(nil, t.TempDir()); err == nil {
		t.Fatal("want error")
	}
}
