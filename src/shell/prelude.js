// rustyrobo shell prelude — a mongo-shell compatible API built on runCommand.
//
// Natives provided by Rust:
//   __n_cmd(db, ejson)  -> ejson result, or "\u0001" + error message
//   __n_print(text)
//   __n_oid()           -> 24-char hex ObjectId
//   __n_sleep(ms)
//   __n_batch()         -> default page size (Robo 3T "batch size")
"use strict";

// ---------------------------------------------------------------- BSON types

class ObjectIdT {
    constructor(hex) {
        if (hex instanceof ObjectIdT) hex = hex.str;
        if (hex === undefined || hex === null) hex = __n_oid();
        hex = String(hex);
        if (!/^[0-9a-fA-F]{24}$/.test(hex)) throw new Error("Invalid ObjectId: " + hex);
        this.str = hex.toLowerCase();
    }
    getTimestamp() { return new Date(parseInt(this.str.substring(0, 8), 16) * 1000); }
    toHexString() { return this.str; }
    valueOf() { return this.str; }
    toString() { return 'ObjectId("' + this.str + '")'; }
    equals(o) { return o instanceof ObjectIdT && o.str === this.str; }
    tojson() { return this.toString(); }
}
globalThis.ObjectId = function (hex) { return new ObjectIdT(hex); };
globalThis.ObjectId.prototype = ObjectIdT.prototype;

globalThis.ObjectId.fromDate = function (d) {
    const secs = Math.floor(new Date(d).getTime() / 1000).toString(16).padStart(8, "0");
    return new ObjectIdT(secs + "0000000000000000");
};
globalThis.ObjectId.isValid = function (s) { return /^[0-9a-fA-F]{24}$/.test(String(s)); };

class NumberLongT {
    constructor(v) {
        if (v === undefined) v = 0;
        if (typeof v === "bigint") v = v.toString();
        if (typeof v === "number") v = Math.trunc(v).toString();
        v = String(v);
        if (!/^-?\d+$/.test(v)) throw new Error("Invalid NumberLong: " + v);
        this.s = BigInt(v).toString();
    }
    valueOf() { return Number(this.s); }
    toNumber() { return Number(this.s); }
    toString() { return this.s; }
    tojson() { return "NumberLong(" + (Math.abs(Number(this.s)) < 2 ** 53 ? this.s : '"' + this.s + '"') + ")"; }
}
globalThis.NumberLong = (v) => new NumberLongT(v);
globalThis.Long = globalThis.NumberLong;
globalThis.NumberLong.prototype = NumberLongT.prototype;

class NumberIntT {
    constructor(v) { this.v = (Number(v) | 0); }
    valueOf() { return this.v; }
    toString() { return String(this.v); }
    tojson() { return "NumberInt(" + this.v + ")"; }
}
globalThis.NumberInt = (v) => new NumberIntT(v);
globalThis.Int32 = globalThis.NumberInt;
globalThis.NumberInt.prototype = NumberIntT.prototype;

class DoubleT {
    constructor(v) { this.v = Number(v); }
    valueOf() { return this.v; }
    toString() { return String(this.v); }
    tojson() { return "Double(" + this.v + ")"; }
}
globalThis.Double = (v) => new DoubleT(v);
globalThis.Double.prototype = DoubleT.prototype;

class NumberDecimalT {
    constructor(v) { this.s = v === undefined ? "0" : String(v); }
    valueOf() { return Number(this.s); }
    toString() { return this.s; }
    tojson() { return 'NumberDecimal("' + this.s + '")'; }
}
globalThis.NumberDecimal = (v) => new NumberDecimalT(v);
globalThis.Decimal128 = globalThis.NumberDecimal;
globalThis.NumberDecimal.prototype = NumberDecimalT.prototype;

const __B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
function __hexToB64(hex) {
    const bytes = [];
    for (let i = 0; i < hex.length; i += 2) bytes.push(parseInt(hex.substr(i, 2), 16));
    let out = "";
    for (let i = 0; i < bytes.length; i += 3) {
        const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
        out += __B64[(n >> 18) & 63] + __B64[(n >> 12) & 63];
        out += i + 1 < bytes.length ? __B64[(n >> 6) & 63] : "=";
        out += i + 2 < bytes.length ? __B64[n & 63] : "=";
    }
    return out;
}
function __b64ToHex(b64) {
    let bits = 0, acc = 0, hex = "";
    for (const ch of b64.replace(/=+$/, "")) {
        acc = (acc << 6) | __B64.indexOf(ch);
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            hex += ((acc >> bits) & 255).toString(16).padStart(2, "0");
        }
    }
    return hex;
}

class BinDataT {
    constructor(subtype, base64) { this.sub = Number(subtype); this.b64 = String(base64); }
    base64() { return this.b64; }
    hex() { return __b64ToHex(this.b64); }
    subtype() { return this.sub; }
    length() { return this.hex().length / 2; }
    toString() { return this.tojson(); }
    tojson() {
        if (this.sub === 4 && this.length() === 16) {
            const h = this.hex();
            return 'UUID("' + h.substr(0, 8) + "-" + h.substr(8, 4) + "-" + h.substr(12, 4) + "-" + h.substr(16, 4) + "-" + h.substr(20) + '")';
        }
        return "BinData(" + this.sub + ',"' + this.b64 + '")';
    }
}
globalThis.BinData = (s, b) => new BinDataT(s, b);
globalThis.BinData.prototype = BinDataT.prototype;
globalThis.HexData = (s, hex) => new BinDataT(s, __hexToB64(hex));
globalThis.UUID = function (s) {
    if (s === undefined) {
        const h = Array.from({ length: 32 }, () => Math.floor(Math.random() * 16).toString(16)).join("");
        s = h.substr(0, 12) + "4" + h.substr(13, 3) + ((parseInt(h[16], 16) & 3) | 8).toString(16) + h.substr(17);
    }
    const hex = String(s).replace(/-/g, "");
    if (!/^[0-9a-fA-F]{32}$/.test(hex)) throw new Error("Invalid UUID: " + s);
    return new BinDataT(4, __hexToB64(hex.toLowerCase()));
};
globalThis.MD5 = (hex) => new BinDataT(5, __hexToB64(hex));

class TimestampT {
    constructor(t, i) {
        if (typeof t === "object" && t !== null) { i = t.i; t = t.t; }
        this.t = Number(t || 0); this.i = Number(i || 0);
    }
    getTime() { return this.t; }
    getInc() { return this.i; }
    toString() { return this.tojson(); }
    tojson() { return "Timestamp(" + this.t + ", " + this.i + ")"; }
}
globalThis.Timestamp = (t, i) => new TimestampT(t, i);
globalThis.Timestamp.prototype = TimestampT.prototype;

class MinKeyT { tojson() { return "MinKey"; } toString() { return "MinKey"; } }
class MaxKeyT { tojson() { return "MaxKey"; } toString() { return "MaxKey"; } }
globalThis.MinKey = new MinKeyT();
globalThis.MaxKey = new MaxKeyT();

class CodeT {
    constructor(code, scope) { this.code = String(code); this.scope = scope; }
    toString() { return this.code; }
    tojson() { return "Code(" + JSON.stringify(this.code) + ")"; }
}
globalThis.Code = (c, s) => new CodeT(c, s);

class BSONRegExpT {
    constructor(pattern, options) { this.pattern = pattern; this.options = options || ""; }
    tojson() { return "/" + this.pattern + "/" + this.options; }
}
globalThis.BSONRegExp = (p, o) => new BSONRegExpT(p, o);

class DBRefT {
    constructor(ns, id, db) { this.$ref = ns; this.$id = id; if (db !== undefined) this.$db = db; }
    tojson() { return "DBRef(" + JSON.stringify(this.$ref) + ", " + tojson(this.$id) + (this.$db ? ", " + JSON.stringify(this.$db) : "") + ")"; }
}
globalThis.DBRef = (ns, id, db) => new DBRefT(ns, id, db);
globalThis.DBPointer = globalThis.DBRef;

globalThis.ISODate = function (s) {
    if (s === undefined) return new Date();
    if (s instanceof Date) return new Date(s.getTime());
    let str = String(s).trim();
    // Accept "2020-01-02 03:04:05" and missing timezone (UTC like the mongo shell).
    if (/^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}(:\d{2}(\.\d+)?)?$/.test(str)) str = str.replace(" ", "T") + "Z";
    else if (/^\d{4}-\d{2}-\d{2}$/.test(str)) str += "T00:00:00Z";
    else str = str.replace(" ", "T");
    const d = new Date(str);
    if (isNaN(d.getTime())) throw new Error("invalid ISO date: " + s);
    return d;
};

// ------------------------------------------------------------- EJSON bridge

function __isPlain(v) {
    if (v === null || typeof v !== "object") return false;
    const p = Object.getPrototypeOf(v);
    return p === Object.prototype || p === null;
}

function __toEJ(v, inArray) {
    if (v === undefined) return inArray ? null : undefined;
    if (v === null) return null;
    switch (typeof v) {
        case "boolean": case "string": return v;
        case "number":
            if (Number.isInteger(v) && v >= -2147483648 && v <= 2147483647 && !Object.is(v, -0))
                return { $numberInt: String(v) };
            if (Number.isNaN(v)) return { $numberDouble: "NaN" };
            if (v === Infinity) return { $numberDouble: "Infinity" };
            if (v === -Infinity) return { $numberDouble: "-Infinity" };
            if (Number.isInteger(v)) return { $numberDouble: v.toFixed(1) };
            return { $numberDouble: String(v) };
        case "bigint": return { $numberLong: v.toString() };
        case "function": return { $code: String(v) };
        case "symbol": return String(v);
    }
    if (v instanceof ObjectIdT) return { $oid: v.str };
    if (v instanceof Date) return { $date: { $numberLong: String(v.getTime()) } };
    if (v instanceof NumberLongT) return { $numberLong: v.s };
    if (v instanceof NumberIntT) return { $numberInt: String(v.v) };
    if (v instanceof DoubleT) return { $numberDouble: Number.isInteger(v.v) ? v.v.toFixed(1) : String(v.v) };
    if (v instanceof NumberDecimalT) return { $numberDecimal: v.s };
    if (v instanceof BinDataT) return { $binary: { base64: v.b64, subType: v.sub.toString(16).padStart(2, "0") } };
    if (v instanceof TimestampT) return { $timestamp: { t: v.t, i: v.i } };
    if (v instanceof MinKeyT) return { $minKey: 1 };
    if (v instanceof MaxKeyT) return { $maxKey: 1 };
    if (v instanceof CodeT) return v.scope ? { $code: v.code, $scope: __toEJ(v.scope) } : { $code: v.code };
    if (v instanceof RegExp) {
        const opts = v.flags.split("").filter((f) => "imsux".includes(f)).sort().join("");
        return { $regularExpression: { pattern: v.source, options: opts } };
    }
    if (v instanceof BSONRegExpT) return { $regularExpression: { pattern: v.pattern, options: v.options.split("").sort().join("") } };
    if (v instanceof DBRefT) {
        const o = { $ref: v.$ref, $id: __toEJ(v.$id) };
        if (v.$db) o.$db = v.$db;
        return o;
    }
    if (v instanceof DBQuery || v instanceof DBCommandCursor) return __toEJ(v.toArray(), inArray);
    if (Array.isArray(v)) return v.map((x) => __toEJ(x, true));
    if (typeof v.toEJSON === "function") return v.toEJSON();
    const out = {};
    for (const k of Object.keys(v)) {
        const x = __toEJ(v[k], false);
        if (x !== undefined) out[k] = x;
    }
    return out;
}

function __fromEJ(v) {
    if (v === null || typeof v !== "object") return v;
    if (Array.isArray(v)) return v.map(__fromEJ);
    const keys = Object.keys(v);
    if (keys.length >= 1 && keys[0][0] === "$") {
        const k = keys[0];
        if (keys.length === 1) {
            const x = v[k];
            switch (k) {
                case "$oid": return new ObjectIdT(x);
                case "$numberInt": return parseInt(x, 10);
                case "$numberDouble": return Number(x);
                case "$numberLong": return new NumberLongT(x);
                case "$numberDecimal": return new NumberDecimalT(x);
                case "$date":
                    if (typeof x === "object" && x.$numberLong !== undefined) return new Date(Number(x.$numberLong));
                    return new Date(x);
                case "$binary": return new BinDataT(parseInt(x.subType, 16), x.base64);
                case "$timestamp": return new TimestampT(x.t, x.i);
                case "$minKey": return MinKey;
                case "$maxKey": return MaxKey;
                case "$code": return new CodeT(x);
                case "$symbol": return x;
                case "$undefined": return undefined;
                case "$regularExpression":
                    try {
                        if (/^[imsu]*$/.test(x.options)) return new RegExp(x.pattern, x.options);
                    } catch (e) { /* fall through */ }
                    return new BSONRegExpT(x.pattern, x.options);
            }
        }
        if (k === "$code" && keys.length === 2 && v.$scope) return new CodeT(v.$code, __fromEJ(v.$scope));
        if (k === "$ref" && v.$id !== undefined) return new DBRefT(v.$ref, __fromEJ(v.$id), v.$db);
    }
    const out = {};
    for (const key of keys) out[key] = __fromEJ(v[key]);
    return out;
}

globalThis.__toEJSONString = (v) => JSON.stringify(__toEJ(v, false) ?? null);

class MongoServerError extends Error {
    constructor(msg) { super(msg); this.name = "MongoServerError"; }
}

function __cmd(dbName, command) {
    const r = __n_cmd(dbName, JSON.stringify(__toEJ(command)));
    if (r.charCodeAt(0) === 1) throw new MongoServerError(r.substring(1));
    return __fromEJ(JSON.parse(r));
}

// ------------------------------------------------------------ pretty print

function __indent(n) { return "    ".repeat(n); }

globalThis.tojson = function tojson(v, indent, nolint, depth) {
    indent = indent || 0; depth = depth || 0;
    if (v === undefined) return "undefined";
    if (v === null) return "null";
    switch (typeof v) {
        case "string": return JSON.stringify(v);
        case "number": return Object.is(v, -0) ? "-0" : (Number.isInteger(v) ? String(v) : String(v));
        case "boolean": return String(v);
        case "bigint": return v.toString() + "n";
        case "function": return String(v);
    }
    if (v instanceof Date) return isNaN(v.getTime()) ? "ISODate(\"Invalid Date\")" : 'ISODate("' + v.toISOString() + '")';
    if (v instanceof RegExp) return String(v);
    if (typeof v.tojson === "function" && !__isPlain(v) && !Array.isArray(v)) return v.tojson();
    if (v instanceof DBQuery || v instanceof DBCommandCursor) return tojson(v.toArray(), indent, nolint, depth);
    if (depth > 100) return "[deep]";
    if (Array.isArray(v)) {
        if (v.length === 0) return "[]";
        if (nolint) return "[ " + v.map((x) => tojson(x, 0, true, depth + 1)).join(", ") + " ]";
        return "[\n" + v.map((x) => __indent(indent + 1) + tojson(x, indent + 1, false, depth + 1)).join(",\n") + "\n" + __indent(indent) + "]";
    }
    const keys = Object.keys(v);
    if (keys.length === 0) return "{}";
    const parts = keys.map((k) => (nolint ? "" : __indent(indent + 1)) + JSON.stringify(k) + " : " + tojson(v[k], indent + 1, nolint, depth + 1));
    if (nolint) return "{ " + parts.join(", ") + " }";
    return "{\n" + parts.join(",\n") + "\n" + __indent(indent) + "}";
};
globalThis.tojsononeline = (v) => tojson(v, 0, true);
globalThis.print = function (...args) {
    __n_print(args.map((a) => (typeof a === "string" ? a : tojson(a))).join(" "));
};
globalThis.printjson = globalThis.printjsononeline = function (v) { __n_print(tojson(v)); };
globalThis.sleep = (ms) => __n_sleep(Number(ms) || 0);
globalThis.isNumber = (x) => typeof x === "number";
globalThis.isString = (x) => typeof x === "string";
globalThis.isObject = (x) => typeof x === "object" && x !== null;
globalThis.hex_md5 = () => { throw new Error("hex_md5 is not supported"); };
globalThis.quit = globalThis.exit = () => { throw new Error("quit() is not available in rustyrobo"); };
globalThis.version = () => "rustyrobo-shell";
globalThis.Object.bsonsize = (o) => JSON.stringify(__toEJ(o)).length;
globalThis.console = { log: print, info: print, warn: print, error: print, dir: printjson };

function __text(s) { return { __shellText: String(s) }; }

// ---------------------------------------------------------------- cursors

function __batch() { return __n_batch(); }

// Cursor options for the initial aggregate command. Without an explicit
// batch size no document is returned yet: the result view reads the cursor
// natively. Pipelines ending in $out / $merge keep the default, because the
// server only runs them with a non-empty first batch.
function __firstBatch(pipeline, cursor) {
    const last = pipeline.length ? Object.keys(pipeline[pipeline.length - 1] || {})[0] : undefined;
    if (cursor.batchSize !== undefined || last === "$out" || last === "$merge") return cursor;
    return Object.assign({}, cursor, { batchSize: 0 });
}

class DBCommandCursor {
    constructor(dbName, res, batchSize) {
        const c = res.cursor;
        this._db = dbName;
        this._buf = c.firstBatch || [];
        this._id = c.id;
        this._ns = c.ns || "";
        this._coll = this._ns.substring(this._ns.indexOf(".") + 1);
        this._batchSize = batchSize;
        // Untouched cursors are read natively by the result view.
        this._touched = false;
    }
    _more() {
        this._touched = true;
        if (this._isDead()) return false;
        const cmd = { getMore: this._id, collection: this._coll };
        if (this._batchSize) cmd.batchSize = this._batchSize;
        const r = __cmd(this._db, cmd);
        this._buf = this._buf.concat(r.cursor.nextBatch || []);
        this._id = r.cursor.id;
        return this._buf.length > 0;
    }
    _isDead() { return this._id === 0 || (this._id instanceof NumberLongT && this._id.s === "0") || this._id === undefined; }
    hasNext() { return this._buf.length > 0 || this._more(); }
    next() {
        if (!this.hasNext()) throw new Error("no more documents in cursor");
        return this._buf.shift();
    }
    tryNext() { return this.hasNext() ? this.next() : null; }
    toArray() { const out = []; while (this.hasNext()) out.push(this.next()); return out; }
    forEach(f) { while (this.hasNext()) f(this.next()); }
    map(f) { const out = []; while (this.hasNext()) out.push(f(this.next())); return out; }
    itcount() { let n = 0; while (this.hasNext()) { this.next(); n++; } return n; }
    objsLeftInBatch() { return this._buf.length; }
    batchSize(n) { this._batchSize = n; return this; }
    pretty() { return this; }
    close() {
        if (!this._isDead()) {
            try { __cmd(this._db, { killCursors: this._coll, cursors: [this._id] }); } catch (e) { /* ignore */ }
            this._id = 0;
        }
    }
    isExhausted() { return this._buf.length === 0 && this._isDead(); }
    [Symbol.iterator]() { const self = this; return { next() { return self.hasNext() ? { value: self.next(), done: false } : { value: undefined, done: true }; } }; }
    // First page for display: the cursor is left positioned after it.
    __page(n) {
        const out = [];
        while (out.length < n && this.hasNext()) out.push(this.next());
        return { docs: out, more: this.hasNext() };
    }
}

class DBQuery {
    constructor(coll, filter, projection) {
        this._coll = coll;
        this._filter = filter === undefined ? {} : filter;
        if (typeof this._filter === "string" || this._filter instanceof ObjectIdT) this._filter = { _id: this._filter };
        this._projection = projection;
        this._sort = undefined;
        this._skip = 0;
        this._limit = 0;
        this._batchSize = undefined;
        this._opts = {};
        this._cursor = null;
    }
    _check() { if (this._cursor) throw new Error("query already executed"); return this; }
    sort(s) { this._check()._sort = s; return this; }
    skip(n) { this._check()._skip = Number(n); return this; }
    limit(n) { this._check()._limit = Number(n); return this; }
    batchSize(n) { this._batchSize = Number(n); return this; }
    projection(p) { this._check()._projection = p; return this; }
    hint(h) { this._opts.hint = h; return this; }
    collation(c) { this._opts.collation = c; return this; }
    maxTimeMS(ms) { this._opts.maxTimeMS = ms; return this; }
    comment(c) { this._opts.comment = c; return this; }
    min(m) { this._opts.min = m; return this; }
    max(m) { this._opts.max = m; return this; }
    returnKey(b) { this._opts.returnKey = b !== false; return this; }
    showRecordId(b) { this._opts.showRecordId = b !== false; return this; }
    showDiskLoc() { return this.showRecordId(); }
    allowDiskUse(b) { this._opts.allowDiskUse = b !== false; return this; }
    noCursorTimeout() { this._opts.noCursorTimeout = true; return this; }
    tailable() { this._opts.tailable = true; return this; }
    readPref() { return this; }
    readConcern(rc) { this._opts.readConcern = { level: rc }; return this; }
    addOption() { return this; }
    pretty() { return this; }
    _command(extra) {
        const cmd = { find: this._coll.getName(), filter: this._filter };
        if (this._projection) cmd.projection = this._projection;
        if (this._sort) cmd.sort = this._sort;
        if (this._skip) cmd.skip = this._skip;
        if (this._limit) {
            cmd.limit = Math.abs(this._limit);
            if (this._limit < 0) cmd.singleBatch = true;
        }
        if (this._batchSize) cmd.batchSize = this._batchSize;
        Object.assign(cmd, this._opts, extra || {});
        return cmd;
    }
    _exec() {
        if (!this._cursor) {
            const dbName = this._coll.getDB().getName();
            this._cursor = new DBCommandCursor(dbName, __cmd(dbName, this._command()), this._batchSize);
        }
        return this._cursor;
    }
    hasNext() { return this._exec().hasNext(); }
    next() { return this._exec().next(); }
    tryNext() { return this._exec().tryNext(); }
    toArray() { return this._exec().toArray(); }
    forEach(f) { return this._exec().forEach(f); }
    map(f) { return this._exec().map(f); }
    itcount() { return this._exec().itcount(); }
    objsLeftInBatch() { return this._exec().objsLeftInBatch(); }
    close() { if (this._cursor) this._cursor.close(); }
    isExhausted() { return this._exec().isExhausted(); }
    [Symbol.iterator]() { return this._exec()[Symbol.iterator](); }
    count(applySkipLimit) {
        const cmd = { count: this._coll.getName(), query: this._filter };
        if (applySkipLimit) {
            if (this._skip) cmd.skip = this._skip;
            if (this._limit) cmd.limit = Math.abs(this._limit);
        }
        if (this._opts.hint) cmd.hint = this._opts.hint;
        if (this._opts.collation) cmd.collation = this._opts.collation;
        return __cmd(this._coll.getDB().getName(), cmd).n;
    }
    size() { return this.count(true); }
    length() { return this.toArray().length; }
    explain(verbosity) {
        const v = verbosity === true ? "allPlansExecution" : (verbosity || "queryPlanner");
        return __cmd(this._coll.getDB().getName(), { explain: this._command(), verbosity: v });
    }
    toString() { return "DBQuery: " + this._coll.getFullName() + " -> " + tojsononeline(this._filter); }
    // Description used by the result view for paging / editing.
    __spec() {
        return {
            db: this._coll.getDB().getName(),
            coll: this._coll.getName(),
            filter: this._filter,
            projection: this._projection || null,
            sort: this._sort || null,
            skip: this._skip,
            limit: this._limit,
            hint: this._opts.hint || null,
            collation: this._opts.collation || null,
        };
    }
}

// --------------------------------------------------------------- results

function __writeResult(r, extra) {
    if (r.writeErrors && r.writeErrors.length) {
        const e = r.writeErrors[0];
        throw new MongoServerError("WriteError: " + e.errmsg + " (code " + e.code + ")");
    }
    if (r.writeConcernError) throw new MongoServerError("WriteConcernError: " + r.writeConcernError.errmsg);
    return Object.assign({ acknowledged: true }, extra);
}

function __withId(doc) {
    if (doc === null || typeof doc !== "object" || Array.isArray(doc)) throw new Error("document must be an object");
    if (doc._id !== undefined) return doc;
    // _id first, as the mongo shell does.
    return Object.assign({ _id: new ObjectIdT() }, doc);
}

function __isUpdateDoc(u) {
    if (Array.isArray(u)) return true;
    const k = Object.keys(u || {});
    return k.length > 0 && k[0][0] === "$";
}

// ------------------------------------------------------------- collection

class DBCollection {
    constructor(db, name) { this._db = db; this._name = name; }
    getName() { return this._name; }
    getFullName() { return this._db.getName() + "." + this._name; }
    getDB() { return this._db; }
    toString() { return this.getFullName(); }
    tojson() { return this.getFullName(); }
    getCollection(sub) { return this._db.getCollection(this._name + "." + sub); }
    _run(cmd) { return __cmd(this._db.getName(), cmd); }

    find(filter, projection) { return new DBQuery(this, filter, projection); }
    findOne(filter, projection, options) {
        const q = new DBQuery(this, filter, projection).limit(-1);
        if (options && options.sort) q.sort(options.sort);
        return q.hasNext() ? q.next() : null;
    }
    aggregate(pipeline, options) {
        if (!Array.isArray(pipeline)) pipeline = Array.prototype.slice.call(arguments), options = {};
        options = Object.assign({}, options || {});
        const cursor = options.cursor || {};
        delete options.cursor;
        if (options.explain) {
            delete options.explain;
            return this._run(Object.assign({ aggregate: this._name, pipeline, explain: true }, options));
        }
        const first = __firstBatch(pipeline, cursor);
        const res = this._run(Object.assign({ aggregate: this._name, pipeline, cursor: first }, options));
        return new DBCommandCursor(this._db.getName(), res, cursor.batchSize);
    }
    watch() { throw new Error("Change streams are not supported in the shell view"); }

    insert(docs, options) {
        const many = Array.isArray(docs);
        const list = (many ? docs : [docs]).map(__withId);
        const ordered = !(options && options.ordered === false);
        const r = this._run({ insert: this._name, documents: list, ordered });
        if (r.writeErrors && r.writeErrors.length) throw new MongoServerError("WriteError: " + r.writeErrors[0].errmsg);
        return many ? { acknowledged: true, insertedIds: list.map((d) => d._id), nInserted: r.n } : { nInserted: r.n };
    }
    insertOne(doc, options) {
        const d = __withId(doc);
        const r = this._run(Object.assign({ insert: this._name, documents: [d] }, options && options.writeConcern ? { writeConcern: options.writeConcern } : {}));
        return __writeResult(r, { insertedId: d._id });
    }
    insertMany(docs, options) {
        const list = docs.map(__withId);
        const r = this._run({ insert: this._name, documents: list, ordered: !(options && options.ordered === false) });
        const ids = {};
        list.forEach((d, i) => (ids[i] = d._id));
        return __writeResult(r, { insertedIds: ids });
    }
    _update(filter, update, opts, multi) {
        opts = opts || {};
        const u = { q: filter || {}, u: update, upsert: !!opts.upsert, multi: !!multi };
        for (const k of ["arrayFilters", "collation", "hint"]) if (opts[k] !== undefined) u[k] = opts[k];
        const cmd = { update: this._name, updates: [u] };
        if (opts.writeConcern) cmd.writeConcern = opts.writeConcern;
        return this._run(cmd);
    }
    update(filter, update, upsertOrOpts, multi) {
        let opts = typeof upsertOrOpts === "object" && upsertOrOpts !== null ? upsertOrOpts : { upsert: !!upsertOrOpts, multi: !!multi };
        const r = this._update(filter, update, opts, opts.multi);
        if (r.writeErrors && r.writeErrors.length) throw new MongoServerError("WriteError: " + r.writeErrors[0].errmsg);
        const nUp = r.upserted ? r.upserted.length : 0;
        const res = { nMatched: r.n - nUp, nUpserted: nUp, nModified: r.nModified };
        if (nUp) res._id = r.upserted[0]._id;
        return res;
    }
    _updateResult(r) {
        const nUp = r.upserted ? r.upserted.length : 0;
        const extra = { matchedCount: r.n - nUp, modifiedCount: r.nModified, upsertedCount: nUp };
        extra.upsertedId = nUp ? r.upserted[0]._id : null;
        return __writeResult(r, extra);
    }
    updateOne(filter, update, opts) {
        if (!__isUpdateDoc(update)) throw new Error("the update operation document must contain atomic operators");
        return this._updateResult(this._update(filter, update, opts, false));
    }
    updateMany(filter, update, opts) {
        if (!__isUpdateDoc(update)) throw new Error("the update operation document must contain atomic operators");
        return this._updateResult(this._update(filter, update, opts, true));
    }
    replaceOne(filter, replacement, opts) {
        if (__isUpdateDoc(replacement)) throw new Error("the replace operation document must not contain atomic operators");
        return this._updateResult(this._update(filter, replacement, opts, false));
    }
    save(doc) {
        if (doc._id === undefined) return this.insert(doc);
        return this.update({ _id: doc._id }, doc, { upsert: true });
    }
    _delete(filter, limit, opts) {
        opts = opts || {};
        const d = { q: filter || {}, limit };
        if (opts.collation) d.collation = opts.collation;
        if (opts.hint) d.hint = opts.hint;
        return this._run({ delete: this._name, deletes: [d] });
    }
    remove(filter, justOne) {
        if (filter === undefined) throw new Error("remove needs a query");
        const one = typeof justOne === "object" && justOne !== null ? !!justOne.justOne : !!justOne;
        const r = this._delete(filter, one ? 1 : 0);
        if (r.writeErrors && r.writeErrors.length) throw new MongoServerError("WriteError: " + r.writeErrors[0].errmsg);
        return { nRemoved: r.n };
    }
    deleteOne(filter, opts) { const r = this._delete(filter, 1, opts); return __writeResult(r, { deletedCount: r.n }); }
    deleteMany(filter, opts) { const r = this._delete(filter, 0, opts); return __writeResult(r, { deletedCount: r.n }); }
    _fam(cmd, opts) {
        opts = opts || {};
        if (opts.sort) cmd.sort = opts.sort;
        if (opts.projection) cmd.fields = opts.projection;
        if (opts.upsert) cmd.upsert = true;
        if (opts.returnNewDocument || opts.returnDocument === "after") cmd.new = true;
        for (const k of ["arrayFilters", "collation", "hint", "maxTimeMS", "writeConcern"]) if (opts[k] !== undefined) cmd[k] = opts[k];
        return this._run(cmd).value;
    }
    findOneAndUpdate(filter, update, opts) { return this._fam({ findAndModify: this._name, query: filter, update }, opts); }
    findOneAndReplace(filter, repl, opts) { return this._fam({ findAndModify: this._name, query: filter, update: repl }, opts); }
    findOneAndDelete(filter, opts) { return this._fam({ findAndModify: this._name, query: filter, remove: true }, opts); }
    findAndModify(spec) {
        const cmd = Object.assign({ findAndModify: this._name }, spec);
        return this._run(cmd).value;
    }
    bulkWrite(ops, options) {
        const res = { acknowledged: true, insertedCount: 0, matchedCount: 0, modifiedCount: 0, deletedCount: 0, upsertedCount: 0, insertedIds: {}, upsertedIds: {} };
        ops.forEach((op, i) => {
            const [kind] = Object.keys(op);
            const a = op[kind];
            switch (kind) {
                case "insertOne": { const r = this.insertOne(a.document); res.insertedCount++; res.insertedIds[i] = r.insertedId; break; }
                case "updateOne": case "updateMany": case "replaceOne": {
                    const r = kind === "replaceOne" ? this.replaceOne(a.filter, a.replacement, a) : this[kind](a.filter, a.update, a);
                    res.matchedCount += r.matchedCount; res.modifiedCount += r.modifiedCount;
                    if (r.upsertedId !== null) { res.upsertedCount++; res.upsertedIds[i] = r.upsertedId; }
                    break;
                }
                case "deleteOne": case "deleteMany": res.deletedCount += this[kind](a.filter, a).deletedCount; break;
                default: throw new Error("unknown bulkWrite operation: " + kind);
            }
        });
        return res;
    }
    initializeOrderedBulkOp() { throw new Error("Bulk API is not supported, use bulkWrite()"); }
    initializeUnorderedBulkOp() { return this.initializeOrderedBulkOp(); }

    count(filter, opts) {
        const cmd = Object.assign({ count: this._name, query: filter || {} }, opts || {});
        return this._run(cmd).n;
    }
    countDocuments(filter, opts) {
        opts = opts || {};
        const pipeline = [{ $match: filter || {} }];
        if (opts.skip) pipeline.push({ $skip: opts.skip });
        if (opts.limit) pipeline.push({ $limit: opts.limit });
        pipeline.push({ $group: { _id: 1, n: { $sum: 1 } } });
        const r = this.aggregate(pipeline).toArray();
        return r.length ? r[0].n : 0;
    }
    estimatedDocumentCount() { return this._run({ count: this._name }).n; }
    distinct(key, filter, opts) {
        return this._run(Object.assign({ distinct: this._name, key, query: filter || {} }, opts || {})).values;
    }
    mapReduce(map, reduce, opts) {
        const cmd = Object.assign({ mapReduce: this._name, map: new CodeT(map), reduce: new CodeT(reduce) }, opts || {});
        if (opts && opts.finalize) cmd.finalize = new CodeT(opts.finalize);
        if (!cmd.out) cmd.out = { inline: 1 };
        if (typeof cmd.out === "string") cmd.out = { replace: cmd.out };
        return this._run(cmd);
    }

    createIndex(keys, options, commitQuorum) {
        const spec = Object.assign({ key: keys }, options || {});
        if (!spec.name) spec.name = Object.keys(keys).map((k) => k + "_" + keys[k]).join("_");
        const cmd = { createIndexes: this._name, indexes: [spec] };
        if (commitQuorum !== undefined) cmd.commitQuorum = commitQuorum;
        this._run(cmd);
        return spec.name;
    }
    ensureIndex(keys, options) { return this.createIndex(keys, options); }
    createIndexes(list, options) { return list.map((k) => this.createIndex(k, options)); }
    getIndexes() {
        try {
            return new DBCommandCursor(this._db.getName(), this._run({ listIndexes: this._name, cursor: {} })).toArray();
        } catch (e) {
            if (/NamespaceNotFound|ns does not exist|26/.test(String(e.message))) return [];
            throw e;
        }
    }
    getIndices() { return this.getIndexes(); }
    getIndexSpecs() { return this.getIndexes(); }
    getIndexKeys() { return this.getIndexes().map((i) => i.key); }
    dropIndex(index) { return this._run({ dropIndexes: this._name, index }); }
    dropIndexes(indexes) { return this._run({ dropIndexes: this._name, index: indexes === undefined ? "*" : indexes }); }
    hideIndex(index) { return this._run({ collMod: this._name, index: Object.assign(typeof index === "string" ? { name: index } : { keyPattern: index }, { hidden: true }) }); }
    unhideIndex(index) { return this._run({ collMod: this._name, index: Object.assign(typeof index === "string" ? { name: index } : { keyPattern: index }, { hidden: false }) }); }
    reIndex() { return this._run({ reIndex: this._name }); }

    drop() {
        try { this._run({ drop: this._name }); return true; } catch (e) {
            if (/ns not found|NamespaceNotFound/.test(e.message)) return false;
            throw e;
        }
    }
    renameCollection(newName, dropTarget) {
        if (typeof newName === "object") { dropTarget = newName.dropTarget; newName = newName.to; }
        return __cmd("admin", { renameCollection: this.getFullName(), to: this._db.getName() + "." + newName, dropTarget: !!dropTarget });
    }
    stats(scale) {
        const opts = typeof scale === "object" && scale !== null ? scale : { scale: scale || 1 };
        try {
            return this._run(Object.assign({ collStats: this._name }, opts));
        } catch (e) {
            // collStats was removed from recent servers: use the $collStats stage.
            const r = this.aggregate([{ $collStats: { storageStats: { scale: opts.scale || 1 }, count: {} } }]).toArray();
            if (!r.length) throw e;
            return Object.assign({ ns: this.getFullName() }, r[0].storageStats);
        }
    }
    dataSize() { return this.stats().size; }
    storageSize() { return this.stats().storageSize; }
    totalIndexSize() { return this.stats().totalIndexSize; }
    totalSize() { const s = this.stats(); return (s.storageSize || 0) + (s.totalIndexSize || 0); }
    isCapped() { const s = this.stats(); return !!s.capped; }
    validate(full) { return this._run({ validate: this._name, full: !!full }); }
    exists() { const r = this._db.getCollectionInfos({ name: this._name }); return r.length ? r[0] : null; }
    getShardVersion() { return __cmd("admin", { getShardVersion: this.getFullName() }); }
    getShardDistribution() {
        const s = this.stats();
        if (!s.sharded) return __text("Collection " + this.getFullName() + " is not sharded");
        const lines = [];
        for (const sh of Object.keys(s.shards || {})) {
            const x = s.shards[sh];
            lines.push("Shard " + sh + " : data " + x.size + "b docs " + x.count + " chunks " + (x.nchunks || "?"));
        }
        lines.push("Totals: data " + s.size + "b docs " + s.count);
        return __text(lines.join("\n"));
    }
    latencyStats() { return this.aggregate([{ $collStats: { latencyStats: {} } }]); }
}

// --------------------------------------------------------------- database

function __proxyColl(coll) {
    return new Proxy(coll, {
        get(t, p, r) {
            if (typeof p === "symbol" || p in t) return Reflect.get(t, p, r);
            if (typeof p === "string" && p[0] !== "_") return __proxyColl(t.getCollection(p));
            return undefined;
        },
    });
}

class DB {
    constructor(name) { this._name = name; }
    getName() { return this._name; }
    toString() { return this._name; }
    tojson() { return this._name; }
    getCollection(name) {
        if (!name || typeof name !== "string") throw new Error("collection name must be a non-empty string");
        return __proxyColl(new DBCollection(this, name));
    }
    getSiblingDB(name) { return __makeDB(name); }
    getSisterDB(name) { return __makeDB(name); }
    getMongo() { return { getDB: (n) => __makeDB(n), getDBNames: () => __cmd("admin", { listDatabases: 1, nameOnly: true }).databases.map((d) => d.name), toString: () => "connection" }; }
    runCommand(cmd, extra) {
        if (typeof cmd === "string") { const o = {}; o[cmd] = 1; cmd = Object.assign(o, extra || {}); }
        return __cmd(this._name, cmd);
    }
    adminCommand(cmd, extra) {
        if (typeof cmd === "string") { const o = {}; o[cmd] = 1; cmd = Object.assign(o, extra || {}); }
        return __cmd("admin", cmd);
    }
    _adminCommand(cmd) { return this.adminCommand(cmd); }
    getCollectionInfos(filter, nameOnly) {
        const cmd = { listCollections: 1, filter: filter || {}, cursor: {} };
        if (nameOnly) cmd.nameOnly = true;
        return new DBCommandCursor(this._name, __cmd(this._name, cmd)).toArray();
    }
    getCollectionNames() { return this.getCollectionInfos({}, true).map((c) => c.name).sort(); }
    createCollection(name, opts) { return __cmd(this._name, Object.assign({ create: name }, opts || {})); }
    createView(name, source, pipeline, opts) { return __cmd(this._name, Object.assign({ create: name, viewOn: source, pipeline }, opts || {})); }
    dropDatabase() { return __cmd(this._name, { dropDatabase: 1 }); }
    stats(scale) { return __cmd(this._name, typeof scale === "object" ? Object.assign({ dbStats: 1 }, scale) : { dbStats: 1, scale: scale || 1 }); }
    version() { return __cmd("admin", { buildInfo: 1 }).version; }
    serverBuildInfo() { return __cmd("admin", { buildInfo: 1 }); }
    serverStatus(opts) { return __cmd("admin", Object.assign({ serverStatus: 1 }, opts || {})); }
    serverCmdLineOpts() { return __cmd("admin", { getCmdLineOpts: 1 }); }
    hostInfo() { return __cmd("admin", { hostInfo: 1 }); }
    hello() { return __cmd("admin", { hello: 1 }); }
    isMaster() { return __cmd("admin", { isMaster: 1 }); }
    listCommands() { return __cmd("admin", { listCommands: 1 }); }
    currentOp(opts) {
        if (opts === true) opts = { $all: true };
        return __cmd("admin", Object.assign({ currentOp: 1 }, opts || {}));
    }
    killOp(opid) {
        if (opid === undefined) throw new Error("killOp: provide an operation id (see db.currentOp())");
        return __cmd("admin", { killOp: 1, op: opid });
    }
    getProfilingLevel() { return __cmd(this._name, { profile: -1 }).was; }
    getProfilingStatus() { return __cmd(this._name, { profile: -1 }); }
    setProfilingLevel(level, opts) {
        const cmd = { profile: level };
        if (typeof opts === "number") cmd.slowms = opts; else Object.assign(cmd, opts || {});
        return __cmd(this._name, cmd);
    }
    getLogComponents() { return __cmd("admin", { getParameter: 1, logComponentVerbosity: 1 }).logComponentVerbosity; }
    setLogLevel(level, component) {
        return __cmd("admin", { setParameter: 1, logComponentVerbosity: component ? { [component]: { verbosity: level } } : { verbosity: level } });
    }
    fsyncLock() { return __cmd("admin", { fsync: 1, lock: true }); }
    fsyncUnlock() { return __cmd("admin", { fsyncUnlock: 1 }); }
    repairDatabase() { return __cmd(this._name, { repairDatabase: 1 }); }
    getUsers(opts) {
        const r = __cmd(this._name, Object.assign({ usersInfo: 1 }, opts || {}));
        return r.users;
    }
    getUser(name, opts) {
        const r = __cmd(this._name, Object.assign({ usersInfo: { user: name, db: this._name } }, opts || {}));
        return r.users.length ? r.users[0] : null;
    }
    createUser(user, wc) {
        const cmd = Object.assign({ createUser: user.user }, user);
        delete cmd.user;
        if (wc) cmd.writeConcern = wc;
        __cmd(this._name, cmd);
        return __text("Successfully added user: " + tojson({ user: user.user, roles: user.roles }));
    }
    updateUser(name, update) { return __cmd(this._name, Object.assign({ updateUser: name }, update)); }
    changeUserPassword(name, pwd) { return this.updateUser(name, { pwd }); }
    dropUser(name) { __cmd(this._name, { dropUser: name }); return true; }
    dropAllUsers() { return __cmd(this._name, { dropAllUsersFromDatabase: 1 }); }
    grantRolesToUser(name, roles) { return __cmd(this._name, { grantRolesToUser: name, roles }); }
    revokeRolesFromUser(name, roles) { return __cmd(this._name, { revokeRolesFromUser: name, roles }); }
    getRoles(opts) { return __cmd(this._name, Object.assign({ rolesInfo: 1 }, opts || {})).roles; }
    getRole(name, opts) { const r = __cmd(this._name, Object.assign({ rolesInfo: name }, opts || {})).roles; return r.length ? r[0] : null; }
    createRole(role) { const cmd = Object.assign({ createRole: role.role }, role); delete cmd.role; return __cmd(this._name, cmd); }
    dropRole(name) { return __cmd(this._name, { dropRole: name }); }
    auth() { throw new Error("db.auth(): set credentials in the connection settings instead"); }
    logout() { return __cmd(this._name, { logout: 1 }); }
    printCollectionStats(scale) {
        const out = [];
        for (const n of this.getCollectionNames()) {
            out.push(n);
            try { out.push(tojson(this.getCollection(n).stats(scale))); } catch (e) { out.push("  error: " + e.message); }
            out.push("---");
        }
        return __text(out.join("\n"));
    }
    getReplicationInfo() { return rs.printReplicationInfo(); }
    printShardingStatus() { return sh.status(); }
    eval() { throw new Error("db.eval() was removed in MongoDB 4.2"); }
    getLastError() { return null; }
    getLastErrorObj() { return { ok: 1 }; }
    watch() { throw new Error("Change streams are not supported in the shell view"); }
    aggregate(pipeline, options) {
        options = Object.assign({}, options || {});
        const cursor = options.cursor || {};
        delete options.cursor;
        const first = __firstBatch(pipeline, cursor);
        return new DBCommandCursor(this._name, __cmd(this._name, Object.assign({ aggregate: 1, pipeline, cursor: first }, options)), cursor.batchSize);
    }
}

function __makeDB(name) {
    return new Proxy(new DB(name), {
        get(t, p, r) {
            if (typeof p === "symbol" || p in t) return Reflect.get(t, p, r);
            if (typeof p === "string" && p[0] !== "_") return t.getCollection(p);
            return undefined;
        },
    });
}
globalThis.__makeDB = __makeDB;
globalThis.DB = DB;
globalThis.DBCollection = DBCollection;
globalThis.DBQuery = DBQuery;
globalThis.DBCommandCursor = DBCommandCursor;

// --------------------------------------------------------------- rs / sh

globalThis.rs = {
    status: () => __cmd("admin", { replSetGetStatus: 1 }),
    conf: () => __cmd("admin", { replSetGetConfig: 1 }).config,
    config: () => rs.conf(),
    isMaster: () => __cmd("admin", { isMaster: 1 }),
    hello: () => __cmd("admin", { hello: 1 }),
    initiate: (cfg) => __cmd("admin", cfg ? { replSetInitiate: cfg } : { replSetInitiate: {} }),
    reconfig: (cfg, opts) => {
        const cur = rs.conf();
        cfg.version = (cur.version || 0) + 1;
        return __cmd("admin", Object.assign({ replSetReconfig: cfg }, opts || {}));
    },
    add: (host) => {
        const cfg = rs.conf();
        const m = typeof host === "string" ? { host } : host;
        m._id = m._id !== undefined ? m._id : Math.max(-1, ...cfg.members.map((x) => x._id)) + 1;
        cfg.members.push(m);
        return rs.reconfig(cfg);
    },
    remove: (host) => {
        const cfg = rs.conf();
        cfg.members = cfg.members.filter((m) => m.host !== host);
        return rs.reconfig(cfg);
    },
    stepDown: (secs, catchUp) => __cmd("admin", { replSetStepDown: secs || 60, secondaryCatchUpPeriodSecs: catchUp || 10 }),
    freeze: (secs) => __cmd("admin", { replSetFreeze: secs }),
    syncFrom: (host) => __cmd("admin", { replSetSyncFrom: host }),
    printReplicationInfo: () => {
        const local = __makeDB("local");
        const st = local.getCollection("oplog.rs").stats();
        const first = local.getCollection("oplog.rs").find().sort({ $natural: 1 }).limit(1).toArray()[0];
        const last = local.getCollection("oplog.rs").find().sort({ $natural: -1 }).limit(1).toArray()[0];
        const lines = ["configured oplog size: " + (st.maxSize / 1048576).toFixed(2) + "MB"];
        if (first && last) {
            const span = last.ts.t - first.ts.t;
            lines.push("log length start to end: " + span + "secs (" + (span / 3600).toFixed(2) + "hrs)");
            lines.push("oplog first event time: " + new Date(first.ts.t * 1000).toISOString());
            lines.push("oplog last event time: " + new Date(last.ts.t * 1000).toISOString());
        }
        return __text(lines.join("\n"));
    },
    printSecondaryReplicationInfo: () => {
        const st = rs.status();
        const primary = st.members.find((m) => m.stateStr === "PRIMARY");
        return __text(st.members.filter((m) => m.stateStr === "SECONDARY").map((m) =>
            "source: " + m.name + "\n\tsyncedTo: " + m.optimeDate.toISOString() +
            (primary ? "\n\t" + ((primary.optimeDate - m.optimeDate) / 1000) + " secs behind the primary" : "")).join("\n"));
    },
    help: () => __text("rs.status(), rs.conf(), rs.initiate(cfg), rs.add(host), rs.remove(host), rs.stepDown(), rs.printReplicationInfo()"),
};

globalThis.sh = {
    status: () => {
        const cfg = __makeDB("config");
        const lines = ["--- Sharding Status ---", "  shards:"];
        for (const s of cfg.getCollection("shards").find().toArray()) lines.push("    " + tojsononeline(s));
        lines.push("  databases:");
        for (const d of cfg.getCollection("databases").find().sort({ _id: 1 }).toArray()) lines.push("    " + tojsononeline(d));
        return __text(lines.join("\n"));
    },
    enableSharding: (dbName) => __cmd("admin", { enableSharding: dbName }),
    shardCollection: (ns, key, unique) => __cmd("admin", { shardCollection: ns, key, unique: !!unique }),
    addShard: (url) => __cmd("admin", { addShard: url }),
    getBalancerState: () => __cmd("admin", { balancerStatus: 1 }).mode !== "off",
    isBalancerRunning: () => __cmd("admin", { balancerStatus: 1 }).inBalancerRound,
    startBalancer: () => __cmd("admin", { balancerStart: 1 }),
    stopBalancer: () => __cmd("admin", { balancerStop: 1 }),
    help: () => __text("sh.status(), sh.enableSharding(db), sh.shardCollection(ns, key), sh.addShard(url)"),
};

// ----------------------------------------------------------- shell helpers

function __fmtSize(bytes) {
    return (bytes / 1073741824).toFixed(3) + "GB";
}

globalThis.__show = function (what, arg) {
    switch (what) {
        case "dbs": case "databases": {
            const r = __cmd("admin", { listDatabases: 1 });
            const w = Math.max(0, ...r.databases.map((d) => d.name.length));
            return __text(r.databases.map((d) => d.name.padEnd(w + 2) + __fmtSize(d.sizeOnDisk || 0)).join("\n"));
        }
        case "collections": case "tables": return __text(db.getCollectionNames().join("\n"));
        case "users": return db.getUsers();
        case "roles": return db.getRoles({ showBuiltinRoles: true });
        case "profile": return db.getCollection("system.profile").find({ millis: { $gt: 0 } }).sort({ $natural: -1 }).limit(5);
        case "log": {
            const r = __cmd("admin", { getLog: arg || "global" });
            return __text((r.log || []).join("\n"));
        }
        case "logs": return __text(__cmd("admin", { getLog: "*" }).names.join("\n"));
        default: throw new Error("don't know how to show [" + what + "]");
    }
};

globalThis.__use = function (name) {
    globalThis.db = __makeDB(name);
    return __text("switched to db " + name);
};

globalThis.__it = function () {
    const q = globalThis.__lastQuery;
    if (!q) throw new Error("no cursor");
    return q;
};

globalThis.help = function () {
    return __text([
        "db.getCollection('name').find({...})   query a collection",
        "db.name.insertOne({...}) / insertMany([...])",
        "db.name.updateOne(filter, update) / updateMany / replaceOne",
        "db.name.deleteOne(filter) / deleteMany(filter)",
        "db.name.aggregate([...])",
        "db.name.createIndex({field: 1}) / getIndexes() / dropIndex(name)",
        "db.stats(), db.serverStatus(), db.currentOp(), db.getUsers()",
        "show dbs | show collections | show users | show log | use <db>",
        "rs.status(), rs.conf(), sh.status()",
        "print(), printjson(), tojson()",
    ].join("\n"));
};

// Converts the completion value of a statement into a result descriptor
// for the Rust side.
globalThis.__shellResult = function (v) {
    const N = __batch();
    if (v === undefined) return JSON.stringify({ t: "none" });
    if (v !== null && typeof v === "object" && v.__shellText !== undefined) return JSON.stringify({ t: "text", text: v.__shellText });
    if (v instanceof DBQuery) {
        globalThis.__lastQuery = v;
        const spec = v.__spec();
        // Not executed yet: Rust runs the find itself and keeps the documents
        // as BSON (no EJSON round trip through JavaScript).
        if (!v._cursor) return JSON.stringify({ t: "query", spec: __toEJ(spec) });
        const page = v._exec().__page(spec.limit && spec.limit > 0 && spec.limit < N ? spec.limit : N);
        return JSON.stringify({ t: "docs", docs: __toEJ(page.docs), spec: __toEJ(spec), more: page.more });
    }
    if (v instanceof DBCommandCursor) {
        if (!v._touched && v._buf.length === 0 && !v._isDead())
            return JSON.stringify({ t: "cursor", db: v._db, coll: v._coll, ns: v._db + "." + v._coll, id: __toEJ(v._id) });
        const page = v.__page(N);
        v.close();
        return JSON.stringify({ t: "docs", docs: __toEJ(page.docs), ns: v._db + "." + v._coll, more: page.more });
    }
    if (v instanceof DB) return JSON.stringify({ t: "text", text: v.getName() });
    if (v instanceof DBCollection) return JSON.stringify({ t: "text", text: v.getFullName() });
    if (typeof v === "function") return JSON.stringify({ t: "text", text: String(v) });
    if (Array.isArray(v) && v.length > 0 && v.every((x) => __isPlain(x))) return JSON.stringify({ t: "docs", docs: __toEJ(v), more: false });
    if (v !== null && typeof v === "object" && !(v instanceof Date) && !(v instanceof RegExp) && (typeof v.tojson !== "function" || __isPlain(v)))
        return JSON.stringify({ t: "docs", docs: [__toEJ(v)], more: false });
    if (typeof v === "string") return JSON.stringify({ t: "text", text: v });
    return JSON.stringify({ t: "text", text: tojson(v) });
};

globalThis.__fmtErr = function (e) {
    if (e instanceof Error) {
        let s = (e.name && e.name !== "Error" ? e.name + ": " : "Error: ") + e.message;
        if (!(e instanceof MongoServerError) && e.stack) {
            const frames = String(e.stack).split("\n").filter((l) => l.trim() && !l.includes("<prelude>")).slice(0, 5);
            if (frames.length) s += "\n" + frames.join("\n");
        }
        return s;
    }
    return "Error: " + tojson(e);
};

globalThis.db = __makeDB("test");
