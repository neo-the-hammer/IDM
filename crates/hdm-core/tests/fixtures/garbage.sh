#!/bin/sh
# Pretends to be a plugin host that answers with something that is not JSON.
if [ "$1" = "-c" ]; then echo 0.1.0; exit 0; fi
cat > /dev/null
echo 'this is not json'
