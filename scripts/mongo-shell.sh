#!/bin/sh
set -eu

exec mongosh \
    'mongodb://127.0.0.1:27017/brnavi_auth?authSource=admin' \
    --username brnavi-admin \
    --password
