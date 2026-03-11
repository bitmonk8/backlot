# Flick shell

const script_dir = path self | path dirname

if "FLICK_SHELL" not-in $env {
    $env.FLICK_SHELL = "1"
    const self_path = path self
    ^nu --env-config $self_path
    exit
}

cd $script_dir

$env.PATH = ($env.PATH | prepend ([$script_dir, "target", "debug"] | path join))

print "Ready."
