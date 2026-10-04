// Wire types of the amail HTTP API (`/v1`), as the web client uses them.

export type SeatState = 'offline' | 'busy' | 'idle' | 'awaiting' | 'dormant';
export const SEAT_STATES: SeatState[] = ['busy', 'awaiting', 'idle', 'dormant', 'offline'];

export type Source = 'user' | 'agent' | 'external' | 'system';
export type DeliveryClass = 'to' | 'cc' | 'list';
export type DeliveryMode = 'immediate' | 'digest' | 'archive';
export type Priority = 'normal' | 'urgent';
export type TokenScope = 'user' | 'seat' | 'node' | 'operator';
export type PostPolicy = 'anyone' | 'members' | 'user_only';
export type LinkState = 'requested' | 'invited' | 'active';
export type PermissionMode = 'default' | 'unattended';
export type Meta = Record<string, string>;

export interface WhoAmI {
  org: string;
  scope: TokenScope;
  seat?: string;
}

export interface Presence {
  state: SeatState;
  wake_failed?: boolean;
  harness?: string;
  node?: string;
  since: string;
  updated_at: string;
  attention?: string;
  attention_hint?: string;
  terminal_url?: string;
  node_connected?: boolean;
}

export interface Seat {
  name: string;
  kind: 'agent' | 'user';
  parent?: string;
  display_name?: string;
  role?: string;
  /** In `GET /v1/seats`: `role` holds only its beginning; `GET /v1/seats/{name}` has all of it. */
  role_truncated?: boolean;
  harness?: string;
  model?: string;
  permission_mode?: PermissionMode;
  group?: string;
  created_at: string;
  presence?: Presence;
  /** In focus mode (only urgent mail reaches it) until then. */
  focus_until?: string;
}

export interface PresenceEvent extends Presence {
  seat: string;
}

export interface MailboxEvent {
  seat: string;
  seq: number;
}

export interface MessageHeader {
  id: number;
  thread_id: number;
  in_reply_to?: number;
  from: string;
  source: Source;
  to: string[];
  cc: string[];
  subject: string;
  priority: Priority;
  ack_required: boolean;
  lines: number;
  created_at: string;
  meta?: Meta;
  /** The server's retention period removed the body. */
  expired?: boolean;
}

export interface Message extends MessageHeader {
  body: string;
  /** In a thread: the body was left out to keep the answer small; fetch it with `GET /v1/messages/{id}`. */
  body_omitted?: boolean;
}

export interface Delivery {
  seat: string;
  seq: number;
  mod_seq: number;
  class: DeliveryClass;
  mode: DeliveryMode;
  notified_at?: string;
  notified_via?: string;
  read_at?: string;
  acked_at?: string;
}

export interface InboxEntry {
  delivery: Delivery;
  message: MessageHeader;
}

export interface InboxResponse {
  seat: string;
  entries: InboxEntry[];
  /** The page was cut by size: more entries follow. */
  truncated?: boolean;
}

export interface MessageResponse {
  message: Message;
  delivery?: Delivery;
}

export interface RecipientStatus extends Delivery {
  address: string;
  state: 'queued' | 'notified' | 'read' | 'acked';
  /** Seats behind another org's list, counted without their names. */
  members?: { seats: number; notified: number; read: number; acked: number };
}

export interface StatusResponse {
  message_id: number;
  recipients: RecipientStatus[];
}

export interface ThreadResponse {
  thread_id: number;
  messages: Message[];
  /** Older messages were left out. */
  truncated?: boolean;
}

/** Response of `DELETE /v1/threads/{id}`. */
export interface DeleteMailResponse {
  deleted: number;
  /** Messages that stay: mail exchanged with orgs that still exist. */
  kept: number;
  /** More can be deleted: call again. */
  more: boolean;
}

export interface ArchiveResponse {
  messages: MessageHeader[];
  next_before_id?: number;
}

export interface SearchHit extends MessageHeader {
  snippet: string;
}

export interface SearchResponse {
  messages: SearchHit[];
  next_before_id?: number;
  mode: 'index' | 'scan';
  scan_limit?: number;
}

export interface SendRequest {
  to: string[];
  cc: string[];
  subject: string;
  body: string;
  in_reply_to?: number;
  priority?: Priority;
  ack_required?: boolean;
  meta?: Meta;
}

export interface SendResponse {
  id: number;
  thread_id: number;
  recipients: { address: string; seats: number }[];
  rejected: { address: string; reason: string }[];
  deliveries: number;
}

export interface Overview {
  seats: number;
  states: Partial<Record<SeatState, number>>;
  needs_person: number;
  wake_failed: number;
  mail_last_hour: number;
  mail_last_day: number;
  user_unread: number;
  attention: number;
}

export interface SeatCounts {
  seat: string;
  unread: number;
  pending: number;
  seq: number;
  focus_until?: string;
}

export interface CountsResponse {
  seats: SeatCounts[];
}

export type AttentionKind = 'ack' | 'reply' | 'seat';

export interface AttentionItem {
  kind: AttentionKind;
  since: string;
  seat?: string;
  message?: MessageHeader;
  delivery?: Delivery;
  state?: SeatState;
  attention?: string;
  hint?: string;
  wake_failed?: boolean;
}

export interface ListInfo {
  name: string;
  description?: string;
  post_policy: PostPolicy;
  member_count: number;
  managed?: boolean;
  members?: { address: string; mode: DeliveryMode }[];
}

export interface Link {
  org: string;
  state: LinkState;
}

export interface OrgInfo {
  name: string;
  send_rate_per_min: number;
  send_burst: number;
  max_recipients: number;
  max_thread_depth: number;
  ack_timeout_min: number;
  focus_max_min: number;
  digest_interval_min: number;
  webhook_url?: string;
}

export interface TokenInfo {
  id: number;
  scope: TokenScope;
  seat?: string;
  label?: string;
  created_at: string;
}

export interface TokenResponse {
  token: string;
  scope: TokenScope;
  seat?: string;
}

export type TranscriptRole = 'user' | 'assistant' | 'tool' | 'system';
export type TranscriptKind = 'text' | 'tool_call' | 'tool_result' | 'notice';

export interface TranscriptEvent {
  ts?: string;
  role: TranscriptRole;
  kind: TranscriptKind;
  text: string;
  tool?: string;
  truncated?: boolean;
  error?: boolean;
}

export type TranscriptState =
  | 'waiting'
  | 'streaming'
  | 'node_offline'
  | 'no_session'
  | 'no_file'
  | 'unknown';

export interface TranscriptStatus {
  state: TranscriptState;
  message?: string;
  harness?: string;
  path?: string;
  session?: string;
  skipped?: number;
}

export interface ApiErrorBody {
  error: { code: string; message: string; rejected?: { address: string; reason: string }[] };
}

/** `GET /v1/info` (no sign-in needed). */
export interface ServerInfo {
  version: string;
  public: boolean;
  registration: boolean;
  retention_days?: number;
}

/** `POST /v1/register`: the user token is shown only here. */
export interface RegisterResponse {
  org: string;
  user_seat: string;
  token: string;
}

/** One org as the operator sees it (no mail content). */
export interface OrgStat {
  name: string;
  created_at: string;
  suspended_at?: string;
  seats: number;
  nodes: number;
  messages: number;
  messages_hour: number;
  messages_day: number;
  last_activity?: string;
}

/** `GET /v1/operator/stats`. */
export interface OperatorStats {
  orgs: number;
  suspended: number;
  seats: number;
  nodes_online: number;
  mail_last_5min: number;
  mail_last_hour: number;
  list: OrgStat[];
}
