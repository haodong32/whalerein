-- v1 kinds 1/2 are handshake/query; neither is a control deduplication record.
CREATE INDEX requests_query_retention ON requests(expires_at,request_id)
    WHERE command_id = '' AND kind IN (1,2);
