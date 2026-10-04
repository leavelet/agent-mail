-- Link requests to org names that do not exist (or are suspended). They are
-- kept and listed like any other request, so that a link request does not
-- tell whether an org of that name exists. Removed when an org of that
-- name is created.
CREATE TABLE link_requests (
    org_id      BIGINT  NOT NULL REFERENCES orgs(id),
    peer_name   TEXT    NOT NULL,
    created_at  BIGINT  NOT NULL,
    PRIMARY KEY (org_id, peer_name)
);
