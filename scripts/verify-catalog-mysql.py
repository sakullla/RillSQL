"""Local MySQL protocol fixture with a large virtual catalog and controlled latency.

Run in a temporary venv with mysql-mimic==3.0.5 installed (fixture / fixture):
  python scripts/verify-catalog-mysql.py --delay 0.5
  cargo test --manifest-path src-tauri/Cargo.toml --locked mysql_catalog_latency_fixture -- --ignored --nocapture

This is a protocol fixture, not a real MySQL server. It binds loopback only.
"""
import argparse
import asyncio
import json
import re

from mysql_mimic import MysqlServer, Session
from mysql_mimic.constants import DEFAULT_SERVER_CAPABILITIES
from mysql_mimic.types import Capabilities
from mysql_mimic.auth import IdentityProvider, User, NativePasswordAuthPlugin


class FixtureIdentityProvider(IdentityProvider):
    async def get_user(self, username):
        if username != "fixture":
            return None
        return User(name=username, auth_string=NativePasswordAuthPlugin.create_auth_string("fixture"), auth_plugin=NativePasswordAuthPlugin.name)


class CatalogSession(Session):
    async def handle_query(self, sql, attrs):
        lower = sql.lower()
        if "information_schema." not in lower:
            return await super().handle_query(sql, attrs)
        await asyncio.sleep(ARGS.delay)
        if "information_schema.schemata" in lower:
            print(json.dumps({"kind": "outline"}), flush=True)
            return [("shop",), ("archive",)], ["schema_name"]
        if "information_schema.tables" in lower:
            if "count(*)" in lower:
                print(json.dumps({"kind": "count"}), flush=True)
                return [(100_000,)], ["COUNT(*)"]
            limit = re.search(r"\blimit\s+(\d+)", lower)
            offset = re.search(r"\boffset\s+(\d+)", lower)
            if not limit or not offset:
                raise ValueError("Catalog pages must specify LIMIT and OFFSET")
            size, start = int(limit[1]), int(offset[1])
            if size > 1000:
                raise ValueError("Unexpected growing-prefix catalog read")
            print(json.dumps({"kind": "page", "limit": size, "offset": start}), flush=True)
            rows = [(f"table_{i:06}", "table", None, "", "") for i in range(start, min(start + size, 100_000))]
            return rows, ["table_name", "kind", "table_rows", "signature", "relation"]
        raise ValueError(f"Unexpected eager metadata read: {sql}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=33067)
    parser.add_argument("--delay", type=float, default=0.5)
    ARGS = parser.parse_args()
    capabilities = DEFAULT_SERVER_CAPABILITIES & ~Capabilities.CLIENT_DEPRECATE_EOF & ~Capabilities.CLIENT_QUERY_ATTRIBUTES
    asyncio.run(MysqlServer(session_factory=CatalogSession, capabilities=capabilities, identity_provider=FixtureIdentityProvider()).serve_forever(host="127.0.0.1", port=ARGS.port))
