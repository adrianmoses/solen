#!/bin/sh
# Ensure /data is writable by the edgeclaw user, then drop privileges.
chown edgeclaw:edgeclaw /data
exec su-exec edgeclaw "$@"
