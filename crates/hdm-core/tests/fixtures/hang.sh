#!/bin/sh
# Pretends to be a plugin host caught in a loop: reads nothing, answers nothing.
if [ "$1" = "-c" ]; then echo 0.1.0; exit 0; fi
sleep 60
