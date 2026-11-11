-- The integration harness uses a separate database in the same local cluster.
-- Docker entrypoint scripts run once, on a fresh data volume.
CREATE DATABASE whalerein_test;
