-- Reverts 0003 on dummy/test databases only. 0001/0002 objects are left intact.
DROP TRIGGER IF EXISTS workspace_memberships_identity_fixed ON workspace_memberships;
DROP TABLE workspace_grants;
DROP FUNCTION itsm_membership_identity_fixed();
DROP FUNCTION itsm_grant_guard();
