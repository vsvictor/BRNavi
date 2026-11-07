#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"

sudo -v
sudo docker compose exec auth-db mongosh --quiet --shell --eval '
const admin = db.getSiblingDB("admin");
const result = admin.auth(
  process.env.MONGO_INITDB_ROOT_USERNAME,
  process.env.MONGO_INITDB_ROOT_PASSWORD
);
if (result.ok !== 1) {
  throw new Error("MongoDB authentication failed");
}
db = db.getSiblingDB("brnavi_auth");
'
