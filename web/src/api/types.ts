// Types come from the generated OpenAPI schema (npm run gen:api); never hand-copied.
import type { components } from './schema';

type S = components['schemas'];
export type Meta = S['Meta'];
export type UatNotice = S['UatNotice'];
export type Session = S['Session'];
export type SessionIssued = S['SessionIssued'];
export type Workspace = S['Workspace'];
export type WorkspacePermissions = S['WorkspacePermissions'];
export type PermissionCode = S['PermissionCode'];
export type ApiErrorCode = S['Error']['error']['code'];
export type ApiErrorBody = S['Error'];
export type TicketStatus = S['TicketStatus'];
export type Dashboard = S['Dashboard'];
export type DashboardPeriodKey = S['DashboardPeriodKey'];
export type TicketFilter = S['TicketFilter'];
export type DashboardAttentionTicket = S['DashboardAttentionTicket'];
