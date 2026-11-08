#!/bin/sh
# Writes tests/fixtures/mongodb-*: dumps of the same small database made by
# the mongodump of each server (100 on MongoDB 8 and 7, which name the buckets
# of a time series differently, and 4.2), read by the tests of src/dump.rs and
# tests/live.rs. Needs the containers of dev/docker-compose.yml; MongoDB 7.0
# runs in a container of its own while the script runs.
set -eu
cd "$(dirname "$0")/.."
out=tests/fixtures
mkdir -p "$out"

seed='
const d = db.getSiblingDB("rr_fixture");
d.dropDatabase();
d.users.insertMany([
  { _id: 1, email: "ada@x", age: 36, name: "Ada" },
  { _id: 2, email: "linus@x", age: 28, name: "Linus" },
  { _id: 3, email: "grace@x", age: 85, name: "Grace" },
]);
d.users.createIndex({ email: 1 }, { unique: true });
d.users.createIndex({ age: -1, name: 1 }, { name: "age_name" });
d.getCollection("a/b c").insertOne({ _id: "slash and space" });
d.getCollection("x+y").insertOne({ _id: "plus" });
d.getCollection("café").insertOne({ _id: "accent" });
d.createCollection("empty");
d.createCollection("capped", { capped: true, size: 4096 });
d.capped.insertOne({ _id: 1 });
d.createView("adults", "users", [{ $match: { age: { $gte: 30 } } }]);
d.system.js.insertOne({ _id: "double", value: new Code("function (x) { return 2 * x }") });
'
timeseries='
d.createCollection("ts", { timeseries: { timeField: "t", metaField: "m" } });
d.ts.insertMany([1, 2, 3].map(i => ({ t: new Date(Date.UTC(2024, 0, 1, 0, 0, i)), m: "sensor", v: i })));
'

dump() { # container, tool directory, auth options
    docker exec "$1" sh -c "rm -rf /tmp/fx && mkdir -p /tmp/fx/gzip \
        && mongodump $3 -d rr_fixture -o /tmp/fx --quiet \
        && mongodump $3 -d rr_fixture --archive=/tmp/fx/rr_fixture.archive --quiet \
        && mongodump $3 -d rr_fixture --gzip -o /tmp/fx/gzip --quiet \
        && mongodump $3 -d rr_fixture --gzip --archive=/tmp/fx/rr_fixture.archive.gz --quiet"
    rm -rf "$out/$2"
    docker cp "$1:/tmp/fx" "$out/$2"
}

auth='-u admin -p secret --authenticationDatabase admin'
docker exec rr-mongo8 mongosh --quiet $auth --eval "$seed $timeseries"
dump rr-mongo8 mongodb-8 "$auth"
docker exec rr-mongo42 mongo --quiet --eval "$seed"
dump rr-mongo42 mongodb-4.2 ""
docker exec rr-mongo8 mongosh --quiet $auth --eval 'db.getSiblingDB("rr_fixture").dropDatabase()'
docker exec rr-mongo42 mongo --quiet --eval 'db.getSiblingDB("rr_fixture").dropDatabase()'

docker run -d --rm --name rr-fixture7 -e MONGO_INITDB_ROOT_USERNAME=admin -e MONGO_INITDB_ROOT_PASSWORD=secret mongo:7.0 >/dev/null
trap 'docker stop rr-fixture7 >/dev/null' EXIT
# Authenticated: only once the restart after the creation of the user is over.
until docker exec rr-fixture7 mongosh --quiet $auth --eval 'db.adminCommand("ping")' >/dev/null 2>&1; do sleep 1; done
docker exec rr-fixture7 mongosh --quiet $auth --eval "$seed $timeseries"
dump rr-fixture7 mongodb-7 "$auth"
