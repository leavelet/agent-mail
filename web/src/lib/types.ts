// Wire types of the amail HTTP API (`/v1`), as the web client uses them.

export type SeatState = 'offline' | 'busy' | 'idle' | 'awaiting' | 'dormant';
export const SEAT_STATES: SeatState[] = ['busy', 'awaiting', 'idle', 'dormant', 'offline'];

export type Source = 'user' | 'agent' | 'external';
export type DeliveryClass = 'to' | 'cc' | 'list';
export type DeliveryMode = 'immediate' | 'digest' | 'archive';
export type Priority = 'normal' | 'urgent';
export type TokenScope = 'user' | 'seat' | 'node';
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
  harness?: string;
  model?: string;
  permission_mode?: PermissionMode;
  group?: string;
  created_at: string;
  presence?: Presence;
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
}

export interface Message extends MessageHeader {
  body: string;
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
}

export interface MessageResponse {
  message: Message;
  delivery?: Delivery;
}

export interface RecipientStatus extends Delivery {
  address: string;
  state: 'queued' | 'notified' | 'read' | 'acked';
}

export interface StatusResponse {
  message_id: number;
  recipients: RecipientStatus[];
}

export interface ThreadResponse {
  thread_id: number;
  messages: Message[];
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
