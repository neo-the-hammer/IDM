#!/bin/sh
# Pretends to be a plugin host that exits cleanly without answering.
if [ "$1" = "-c" ]; then echo 0.1.0; exit 0; fi
exit 0
