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

/** The seat's last turn failed (model or API error, or the session ended
 * during the turn). */
export interface TurnFailure {
  reason: string;
  /** When the last failed turn ended. */
  at: string;
  /** Failed turns in a row. */
  count: number;
  /** The node shows the mail of the failed turns to the seat again; false
   * once it stopped (after three failures in a row): a person must look. */
  retrying: boolean;
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
  turn_failed?: TurnFailure;
  node_connected?: boolean;
  /** The open request the seat sleeps on (a node may report it as a
   * number, `r12`, or in `waiting_request`); not reported by every node. */
  waiting?: number | string | boolean;
  waiting_request?: number;
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
  /** The seat's team (at most one). */
  group?: string;
  /** What the seat does, for others to find it (shown to other orgs when open). */
  description?: string;
  /** A contact of its team: opening the team opens it. */
  contact?: boolean;
  /** Audiences the seat is open to (`linked` or org names), directly or as a team contact. */
  open_to?: string[];
  created_at: string;
  presence?: Presence;
  /** In focus mode (only urgent mail reaches it) until then. */
  focus_until?: string;
}

export interface PresenceEvent extends Presence {
  seat: string;
}

/** A seat or list was created, changed or deleted (user streams). */
export interface DirectoryEvent {
  kind: 'seat' | 'list' | 'team' | 'grant';
  name: string;
  change: 'created' | 'updated' | 'deleted';
}

/** Node event streams open for the org (user streams; sent at once and on change). */
export interface NodesEvent {
  connected: number;
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
  /** The sending org's user wrote this message in the name of `from` (an agent seat). */
  written_by?: string;
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
  /** The sender (or the org's user) withdrew it before it was shown; its mode is then `archive`. */
  withdrawn_at?: string;
}

export interface WithdrawResponse {
  delivery: Delivery;
  /** False when it had been withdrawn before. */
  changed: boolean;
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

/** A recipient's delivery in a message's status. For a seat of another org
 * only its state and the read and ack times are given (no `seq`,
 * `mod_seq`, `notified_at` or `notified_via`). */
export interface RecipientStatus extends Omit<Delivery, 'seq' | 'mod_seq'> {
  seq?: number;
  mod_seq?: number;
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
  /** Seats of other orgs on cross-org messages of this thread: they may be addressed in replies. */
  participants?: Card[];
  /** Older messages were left out. */
  truncated?: boolean;
  /** Tasks of the org linked to this thread, or whose discussion thread it is. */
  tasks?: TaskRef[];
  /** Documents of the org this thread cites (`[d12]`), or whose discussion thread it is. */
  docs?: DocRef[];
  /** Comments of this thread on a document whose discussion thread it is. */
  doc_comments?: ThreadDocComment[];
}

/** A comment on a document, as a thread lists it. */
export interface ThreadDocComment {
  message_id: number;
  /** The document's number. */
  doc: number;
  anchor?: string;
  /** The section's heading path when the comment was written. */
  path?: string;
  /** The current revision no longer has the section. */
  removed?: boolean;
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
  /** `GET /v1/sent?delivery=true`: deliveries per state of each message. */
  delivery?: DeliveryCounts[];
}

/** How many deliveries of one message are in each state (each counted in
 * its most advanced state). */
export interface DeliveryCounts {
  message_id: number;
  queued: number;
  notified: number;
  read: number;
  acked: number;
  withdrawn?: number;
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

/** Why a recipient will not see a message at once. */
export interface RecipientNote {
  seat: string;
  /** `dormant`, `offline`, `focus`, `wake_failed`, `turn_failed` (others may follow). */
  state: string;
  /** Mail of this class and priority starts or wakes the seat now. */
  wakes: boolean;
  /** `focus`: when it ends. */
  until?: string;
}

export interface SendResponse {
  id: number;
  thread_id: number;
  /** `wakes`: for a list in To, how many members are notified at once (which wakes idle ones). */
  recipients: AddressResult[];
  rejected: { address: string; reason: string }[];
  deliveries: number;
  notes?: RecipientNote[];
}

export interface AddressResult {
  address: string;
  seats: number;
  wakes?: number;
  /** Added by the server in Cc: this org's primary contact for another org on the message. */
  added?: boolean;
  added_for?: string;
  note?: string;
}

/** `POST /v1/messages/preview`: what sending would do; nothing is sent. */
export interface SendPreview {
  recipients: AddressResult[];
  rejected: { address: string; reason: string }[];
  deliveries: number;
}

export interface Overview {
  seats: number;
  states: Partial<Record<SeatState, number>>;
  needs_person: number;
  wake_failed: number;
  /** Seats whose last turn failed (older servers: absent). */
  turn_failed?: number;
  /** Seats counted as offline that wait at a prompt before they start
   * (older servers: absent). */
  starting?: number;
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

export type AttentionKind = 'ack' | 'reply' | 'seat' | 'notice' | 'request';

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
  turn_failed?: TurnFailure;
  /** The start of the message, its lines joined. */
  preview?: string;
  /** `request` items: the open request. */
  request?: Request;
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
  /** When the link became active, or when it was requested. */
  since?: string;
  /** The note of the request (`invited`: the other user's text, untrusted). */
  note?: string;
  /** Active links: this org's primary contact for that org (added in Cc when needed). */
  primary?: string;
}

/** A team's own primary contact for one linked org. */
export interface TeamPrimary {
  org: string;
  seat: string;
}

export type OrgVisibility = 'listed-by-name' | 'hidden';
export type Outbound = 'any-seat' | 'contacts-only';
export type CardKind = 'user' | 'agent' | 'team';

/** What another org may know of a seat or team. */
export interface Card {
  /** `name@org` for cards of another org. */
  address: string;
  kind: CardKind;
  description?: string;
}

export interface OrgCard {
  name: string;
  display_name?: string;
  description?: string;
}

/** `GET /v1/orgs/{name}`: another org as this org sees it. */
export interface PeerOrg {
  card: OrgCard;
  link?: Link;
  open_to_you: Card[];
  you_open: Grant[];
  /** This org's primary contact for that org (active link). */
  primary?: string;
}

export interface Team {
  name: string;
  description?: string;
  root?: string;
  parent?: string;
  members: string[];
  contacts: string[];
  open_to?: string[];
  managed?: boolean;
  primaries?: TeamPrimary[];
  /** `human`: every document of the team waits for the user to activate it. */
  doc_activate_by?: string;
  created_at: string;
}

/** An opening: `target` is shown to `audience` and may be written to. */
export interface Grant {
  id: number;
  target: string;
  kind: CardKind;
  /** `linked` or an org name. */
  audience: string;
  /** False: a named org without an active link (the grant waits). */
  active: boolean;
  managed?: boolean;
  created_at: string;
}

export interface NewGrant {
  target: string;
  audience?: string;
}

/** What a change does to what one linked org sees. */
export interface SightChange {
  org: string;
  gains?: string[];
  loses?: string[];
}

export interface GrantChange {
  grant?: Grant;
  changes: SightChange[];
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
  display_name?: string;
  description?: string;
  visibility?: OrgVisibility;
  outbound?: Outbound;
  new_threads_per_hour?: number;
  /** A one-time notice for the user; cleared with `DELETE /v1/org/notice`. */
  notice?: string;
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

// --- Tasks (`/v1/tasks…`), see crates/amail-proto/src/tasks.rs ---

/** The shown state: `ready` is an `open` task whose blockers are all closed. */
export type TaskState = 'ready' | 'open' | 'doing' | 'blocked' | 'review' | 'done' | 'cancelled';
/** A state that can be stored (and set): every shown state but `ready`. */
export type StoredTaskState = Exclude<TaskState, 'ready'>;
export type TaskPriority = 'urgent' | 'high' | 'normal' | 'low';
/** The result of a done task. */
export type TaskResult = 'ok' | 'rejected';

export interface Task {
  num: number;
  title: string;
  /** Empty in listings. */
  body?: string;
  state: TaskState;
  stored_state: StoredTaskState;
  priority: TaskPriority;
  rank: number;
  reporter: string;
  assignee?: string;
  team?: string;
  parent?: number;
  labels?: string[];
  due?: string;
  waiting_on?: string;
  blocked_reason?: string;
  thread?: number;
  /** Blockers that do not satisfy their dependency yet, lowest first. */
  waits_on?: number[];
  /** Those of `waits_on` whose dependency needs an ok result. */
  waits_on_ok?: number[];
  /** The revision: the sequence number of the newest event. */
  rev?: number;
  /** With child tasks: done of not cancelled. */
  progress?: { done: number; total: number };
  result?: TaskResult;
  result_ref?: string;
  result_note?: string;
  result_by?: string;
  result_at?: string;
  /** The id of the record the task was imported from. */
  import_key?: string;
  /** A pending request of this seat to take the task over. */
  steal_requested_by?: string;
  steal_requested_at?: string;
  /** The oldest open request for the person on this task (`r12`): the
   * task is blocked on the person while it is open. */
  blocked_on_human?: number;
  created_at: string;
  updated_at: string;
  closed_at?: string;
}

export interface TaskRef {
  num: number;
  title: string;
  state: TaskState;
  assignee?: string;
  result?: TaskResult;
  /** In `blockers`: the dependency needs an ok result. */
  needs_ok?: boolean;
}

export interface TaskLink {
  thread_id: number;
  linked_by?: string;
  linked_at: string;
}

export type TaskEventKind =
  | 'created'
  | 'changed'
  | 'assigned'
  | 'linked'
  | 'unlinked'
  | 'dependency_added'
  | 'dependency_removed'
  | 'commented'
  | 'reminder_sent'
  | 'unblocked'
  | 'steal_requested'
  | 'steal_declined'
  | 'steal_expired'
  | 'result_withdrawn'
  | 'imported';

export interface TaskEvent {
  seq: number;
  at: string;
  actor?: string;
  written_by?: string;
  kind: TaskEventKind | string;
  field?: string;
  from?: string;
  to?: string;
  note?: string;
  message_id?: number;
}

export interface TaskDetail {
  task: Task;
  blockers: TaskRef[];
  blocked: TaskRef[];
  children: TaskRef[];
  links: TaskLink[];
  events: TaskEvent[];
  event_count: number;
  /** `POST /tasks` with a client key used before: nothing was created. */
  existing?: boolean;
}

export interface TaskEventsPage {
  events: TaskEvent[];
  next_after_seq?: number;
  events_compacted: number;
}

export interface TaskList {
  tasks: Task[];
  next_before?: number;
  truncated?: boolean;
}

export interface OwedItem {
  task: Task;
  seat?: string;
  blocks?: TaskRef;
}

export interface OwedList {
  items: OwedItem[];
  truncated?: boolean;
}

export interface BoardColumn {
  state: TaskState;
  count: number;
  tasks: Task[];
}

export interface SeatLoad {
  seat: string;
  doing: number;
  ready: number;
  blocked: number;
  waiting?: number;
  review?: number;
}

export interface TaskBoard {
  team?: string;
  columns: BoardColumn[];
  load: SeatLoad[];
  stale: Task[];
  overdue: Task[];
  chains: { num: number; waits_on: number[] }[];
}

/** One task of a report section. */
export interface ReportItem {
  task: Task;
  seat?: string;
  waiting_for?: string[];
  dependents?: TaskRef[];
}

export interface ReportSection {
  /** `not_started`, `in_progress`, `review`, `rejected`, `approved`, `parents`, `blocked`. */
  name: string;
  count: number;
  items: ReportItem[];
}

/** `GET /v1/tasks/report`. */
export interface TaskReport {
  team?: string;
  sections: ReportSection[];
}

/** Payload of a server-sent `task` event: a task of the org changed. */
export interface TaskBusEvent {
  num: number;
  change: string;
  fields?: string[];
}

export interface TaskCommentResponse {
  id: number;
  thread_id: number;
  task: number;
}

// --- Documents (`/v1/docs`) ---------------------------------------------------

export type DocStatus = 'draft' | 'active' | 'superseded' | 'archived';

export interface Doc {
  num: number;
  slug?: string;
  kind: string;
  title: string;
  description?: string;
  status: DocStatus;
  owner: string;
  team?: string;
  labels?: string[];
  editors?: string[];
  pinned?: boolean;
  rev: number;
  bytes: number;
  tokens: number;
  sha256: string;
  verified_at?: string;
  verified_by?: string;
  expires_at?: string;
  stale?: boolean;
  superseded_by?: number;
  replaces?: number[];
  supplements?: number;
  parent?: number;
  thread?: number;
  /** Left out in listings. */
  body?: string;
  /** The split advice while the token estimate is above the threshold. */
  advice?: string;
  created_at: string;
  updated_at: string;
}

export interface DocRef {
  num: number;
  slug?: string;
  kind: string;
  title: string;
  description?: string;
  status: DocStatus;
  stale?: boolean;
}

export interface DocSection {
  anchor: string;
  level: number;
  heading: string;
  path: string;
  start: number;
  end: number;
  tokens: number;
}

export interface DocLink {
  src: number;
  src_anchor?: string;
  /** `doc`, `task`, `message`, `thread`, `seat` or `path`. */
  kind: string;
  /** As written: `d12#rollback`, `t3`, `#45`, `thread 7`, `@w1`, `src/main.rs:10`. */
  target: string;
  num?: number;
  anchor?: string;
  title_at_write?: string;
  /** The target's current title. */
  title?: string;
  renamed?: boolean;
  dangling?: boolean;
  stale?: boolean;
}

export interface DocEvent {
  seq: number;
  at: string;
  actor?: string;
  written_by?: string;
  kind: string;
  field?: string;
  from?: string;
  to?: string;
  note?: string;
  message_id?: number;
  rev?: number;
}

export interface DocRevision {
  rev: number;
  title: string;
  author?: string;
  written_by?: string;
  message?: string;
  sha256: string;
  bytes: number;
  at: string;
}

export interface DocComment {
  message_id: number;
  by?: string;
  at: string;
  anchor?: string;
  path?: string;
  /** The section was removed by a later revision. */
  removed?: boolean;
}

export interface DocThreadLink {
  thread_id: number;
  linked_by?: string;
  linked_at: string;
}

export interface DocCandidate {
  doc: DocRef;
  similarity: number;
}

export interface DocMove {
  from: string;
  to: string;
}

export interface DocText {
  rev: number;
  title: string;
  anchor?: string;
  path?: string;
  text: string;
  tokens: number;
}

export interface DocDetail {
  doc: Doc;
  read?: DocText;
  sections: DocSection[];
  parent?: DocRef;
  children?: DocRef[];
  superseded_by?: DocRef;
  replaces?: DocRef[];
  supplements?: DocRef;
  supplemented_by?: DocRef[];
  related?: DocRef[];
  links: DocLink[];
  linked_from: DocLink[];
  threads?: DocThreadLink[];
  tasks?: TaskRef[];
  comments?: DocComment[];
  events: DocEvent[];
  event_count: number;
  existing?: boolean;
  unchanged?: boolean;
  candidates?: DocCandidate[];
  candidates_line?: string;
  dangling?: DocLink[];
  moved?: DocMove[];
}

/** The body of a 409 `stale` answer to a write. */
export interface DocStaleBody {
  rev: number;
  doc: DocDetail;
  since: DocEvent[];
  diff: string;
}

export interface DocList {
  docs: Doc[];
  next_before?: number;
}

export interface DocSearchHit {
  num: number;
  slug?: string;
  anchor?: string;
  path?: string;
  kind: string;
  status: DocStatus;
  title: string;
  stale?: boolean;
  verified_at?: string;
  snippet: string;
  line: string;
}

export interface DocSearchResult {
  hits: DocSearchHit[];
  titles_only?: boolean;
}

export interface DocHistory {
  revisions: DocRevision[];
  events: DocEvent[];
  next_after_seq?: number;
}

export interface DocDiff {
  num: number;
  from: number;
  to: number;
  diff: string;
}

export interface DocKind {
  name: string;
  description?: string;
  required_sections: string[];
  activate_by: 'owner' | 'human';
  in_index: boolean;
  body_cap_bytes: number;
  verify_days?: number;
  builtin?: boolean;
  disabled?: boolean;
  docs: number;
}

export interface DocCommentResponse {
  id: number;
  thread_id: number;
  doc: number;
}

export interface DocBusEvent {
  num: number;
  change: string;
  fields?: string[];
}

// --- Seat handover (`/v1/seats/{name}/handover`, `/worklog`) ------------------

export interface HandoverFields {
  state: string;
  summary: string;
  next: string;
  waiting_on: string;
  traps: string;
  verify: string;
  pointers: string;
}

export type HandoverSource = 'agent' | 'compaction' | 'server';

export interface HandoverNote {
  seat: string;
  rev: number;
  fields: HandoverFields;
  text: string;
  chars: number;
  pointers_filled?: boolean;
  private?: boolean;
  source: HandoverSource;
  draft?: boolean;
  session?: string;
  written_by?: string;
  written_at: string;
  read_by_session?: string;
  read_at?: string;
}

export interface HandoverRevision {
  rev: number;
  written_at: string;
  source: HandoverSource;
  private?: boolean;
  chars: number;
  session?: string;
  written_by?: string;
  read_by_session?: string;
  read_at?: string;
}

export interface HandoverResponse {
  seat: string;
  note?: HandoverNote;
  current_rev: number;
  revisions: HandoverRevision[];
  private?: boolean;
  requested_at?: string;
  requested_by?: string;
}

export interface HandoverRequested {
  seat: string;
  requested_at: string;
  message_id?: number;
}

export interface WorklogEntry {
  id: number;
  seat: string;
  session?: string;
  at: string;
  kind: string;
  text: string;
  task?: number;
  doc?: number;
  source: 'server' | 'node';
}

export interface WorklogPage {
  seat: string;
  entries: WorklogEntry[];
  next_before?: number;
}

export interface WorklogSummary {
  seat: string;
  total: number;
  kinds: { kind: string; count: number }[];
  tasks: { task: number; count: number; last_at: string }[];
  last: WorklogEntry[];
}

// --- requests for the person (M10) ---------------------------------------

export type RequestKind = 'question' | 'permission' | 'plan' | 'merge' | 'attention';
export type RequestClass = 'now' | 'soon' | 'later';
export type RequestState = 'open' | 'answered' | 'expired' | 'withdrawn' | 'superseded';

export interface RequestOption {
  key: string;
  label: string;
  description?: string;
}

export interface RequestAnswer {
  option?: string;
  text?: string;
  /** The user's name, a seat's name, or `deadline` (the default was taken). */
  by: string;
  at: string;
  note?: string;
}

export interface Request {
  num: number;
  kind: RequestKind;
  /** The seat the request is about (the one that waits). */
  seat: string;
  asker?: string;
  task?: number;
  title: string;
  body?: string;
  options?: RequestOption[];
  free_text?: boolean;
  default?: string;
  class: RequestClass;
  state: RequestState;
  deadline?: string;
  answer?: RequestAnswer;
  thread?: number;
  source: string;
  attention?: string;
  rev: number;
  created_at: string;
  answered_at?: string;
}

export interface RequestEvent {
  seq: number;
  at: string;
  kind: string;
  actor?: string;
  note?: string;
  message_id?: number;
}

export interface RequestDetail {
  request: Request;
  events?: RequestEvent[];
  threads?: number[];
  existing?: boolean;
  terminal_url?: string;
}

export interface RequestList {
  requests: Request[];
  truncated?: boolean;
}

/** A `request` event on the user's event stream. */
export interface RequestBusEvent {
  num: number;
  change: string;
  state: RequestState;
  seat: string;
}

/** Answer of `POST /v1/seats/{name}/prompt` and `/interrupt`. */
export interface SeatControlResponse {
  queued: boolean;
  seat: string;
  nodes: number;
}
