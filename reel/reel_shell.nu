# Reel shell

const script_dir = path self | path dirname

if "REEL_SHELL" not-in $env {
    # Re-exec as interactive shell with this file as env config
    $env.REEL_SHELL = "1"
    const self_path = path self
    ^nu --env-config $self_path
    exit
}

cd $script_dir

print "Ready."
