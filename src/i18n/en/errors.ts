/** Backend error-code copy domain (T6-A2/R6): keys mirror the stable codes in src-tauri error_codes.rs. */
export const errors = {
  conn: {
    stage_failed: {
      message: "Connection failed",
      hint: "Check the per-stage results to locate the failure.",
    },
    auth_failed: {
      message: "Database authentication failed",
      hint: "Check the database username and password.",
    },
    redis_auth_failed: {
      message: "Redis authentication failed",
      hint: "Check the Redis password or ACL username.",
    },
    network_failed: {
      message: "Network connection failed",
      hint: "No encrypted-connection retry was attempted.",
    },
    tls_terminated: {
      message: "Connection terminated",
      hint: "No downgrade to unencrypted or unverified mode was attempted.",
    },
    tls_downgrade_refused: {
      message: "TLS verification failed",
      hint: "No fallback to an unverified connection.",
    },
    tls_ca_missing: {
      message: "CA certificate file missing",
      hint: "Enter the CA certificate file path.",
    },
    tls_ca_unreadable: {
      message: "CA certificate file unreadable",
      hint: "Check the file path and read permission.",
    },
    tls_cert_missing: {
      message: "Client certificate or key missing",
      hint: "Client certificate and private key must be provided together.",
    },
    tls_cert_unreadable: {
      message: "Client certificate or key unreadable",
      hint: "Check the file paths and read permission.",
    },
    tls_not_configured: {
      message: "TLS configuration incomplete",
      hint: "Complete the encryption settings and retry.",
    },
    version_read_failed: {
      message: "Database version could not be read",
      hint: "Authentication passed but the version read failed.",
    },
    schema_read_failed: {
      message: "Table catalog could not be read",
      hint: "The file is open but reading the catalog failed.",
    },
    sqlite_file_missing: {
      message: "Choose a database file",
      hint: "SQLite opens existing files only.",
    },
    sqlite_open_failed: {
      message: "SQLite file failed to open",
      hint: "The file was not opened; later stages did not run.",
    },
    foreign_keys_read_failed: {
      message: "foreign_keys state could not be read",
      hint: "The file is open but reading the switch failed.",
    },
    ssh_auth_failed: {
      message: "SSH jump host login failed",
      hint: "Check the SSH user, private key or passphrase.",
    },
    ssh_key_unreadable: {
      message: "Private key unreadable or locked",
      hint: "The key passphrase only unlocks the key and is never stored.",
    },
    ssh_not_configured: {
      message: "SSH configuration missing",
      hint: "Not run.",
    },
    tunnel_failed: {
      message: "SSH tunnel not established",
      hint: "The jump host was reached but the channel to the database was not.",
    },
    tunnel_not_ready: {
      message: "Tunnel not ready",
      hint: "The database has not been contacted yet.",
    },
    host_key_changed: {
      message: "SSH host key changed",
      hint: "The connection was blocked and no credentials were sent. Verify the new fingerprint, then forget the saved key and test again.",
    },
    host_key_inconsistent: {
      message: "SSH host key state inconsistent",
      hint: "Authentication did not continue.",
    },
    route_resolve_failed: {
      message: "Connection route resolution failed",
      hint: "Certificates are still verified against the database hostname, not the forwarder address.",
    },
    password_required: {
      message: "Enter the database password",
      hint: "No password is available from the credential store and none was typed. No empty password was saved.",
    },
    sqlite_not_remote: {
      message: "SQLite does not use remote connections",
      hint: "Not run.",
    },
    not_started: {
      message: "Connection did not start",
      hint: "Not run.",
    },
    probe_failed: {
      message: "Liveness probe failed",
      hint: "The connection was not established as a usable session.",
    },
    version_unsupported: {
      message: "Database version not supported",
      hint: "The dialect targets supported versions; older connections stop at the test stage.",
    },
    unknown_version: {
      message: "Database version unrecognized",
      hint: "Confirm the connection points at a supported database.",
    },
  },
  ssh: {
    host_key_wait: {
      message: "Waiting for SSH host key confirmation",
      hint: "Verify the fingerprint through a trusted channel before continuing.",
    },
  },
  import: {
    passphrase_required: {
      message: "This file is passphrase-encrypted; enter the passphrase set at export time.",
      hint: "",
    },
    bad_passphrase: {
      message: "Wrong passphrase or damaged file; the import was rejected as a whole.",
      hint: "",
    },
    unrecognized: {
      message: "Unrecognized file: not a valid RillSQL connections export.",
      hint: "",
    },
  },
  dump: {
    engine_unsupported: {
      message: "This engine has no full-database export; use its dedicated browser.",
      hint: "",
    },
    connection_mismatch: {
      message: "Connection type and engine do not match.",
      hint: "",
    },
    session_gap: {
      message: "The current connection has another database open; no separate connection was made to read this one.",
      hint: "",
    },
    session_gap_main: {
      message: "The current connection has main open; no separate connection was made to read another database.",
      hint: "",
    },
    database_required: {
      message: "A database name is required; no separate connection was made.",
      hint: "",
    },
  },
  migrate: {
    not_queryable: {
      message: "This connection has no queryable database attached.",
      hint: "",
    },
    redis_not_target: {
      message: "Redis is not a migration target; data goes through key commands.",
      hint: "",
    },
    mongo_not_target: {
      message: "MongoDB is not a migration target; documents go through the collection browser.",
      hint: "",
    },
    target_not_table: {
      message: "The target must be an existing table; views cannot be migration targets.",
      hint: "",
    },
    no_mapped_columns: {
      message: "No columns to migrate: the column names do not line up; map them manually.",
      hint: "",
    },
    duplicate_mapping: {
      message: "Two source columns are mapped to the same target column.",
      hint: "",
    },
    source_read_failed: {
      message: "Reading the source table failed.",
      hint: "",
    },
    target_read_failed: {
      message: "Reading the target table failed.",
      hint: "",
    },
    table_missing: {
      message: "The target table does not exist.",
      hint: "",
    },
  },
};
