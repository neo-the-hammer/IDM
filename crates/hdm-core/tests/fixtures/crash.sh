#!/bin/sh
# Pretends to be a plugin host that dies on startup with a real reason.
if [ "$1" = "-c" ]; then echo 0.1.0; exit 0; fi
echo 'ImportError: no module named hdm_plugins' >&2
exit 1
