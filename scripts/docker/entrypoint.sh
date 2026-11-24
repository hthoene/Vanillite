#!/bin/sh
# Entrypoint of the server image.
#
# Pterodactyl (Wings) passes the egg's startup command in STARTUP, with {{VARIABLE}} placeholders,
# and mounts the server folder at /home/container. Without STARTUP the arguments go to the
# server binary (default: run --dir /data).
if [ -n "${STARTUP:-}" ]; then
    cd /home/container || exit 1
    command=$(printf '%s' "$STARTUP" | sed -e 's/{{/${/g' -e 's/}}/}/g')
    printf ':/home/container$ %s\n' "$command"
    # The server replaces the shell, so it gets the signals and the console.
    eval "exec $command"
fi
exec /vanillite "$@"
