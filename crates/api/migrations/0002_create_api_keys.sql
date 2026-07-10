CREATE TABLE api_keys (
    key_hash TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    label TEXT NOT NULL DEFAULT 'default',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_api_keys_tenant
    ON api_keys (tenant_id);

-- No seed keys: mint them via POST /admin/tenants and /admin/tenants/{id}/keys.