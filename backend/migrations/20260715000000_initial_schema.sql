-- NetRoll's complete schema: accounts and their auth, net definitions with schedules
-- and connections, the append-only session event log and its projections, and the
-- delivery queue. Every index is plain-form: this only ever runs against an empty database.

--
-- Name: pg_trgm; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pg_trgm;


--
-- Name: abuse_reports; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.abuse_reports (
    id uuid NOT NULL,
    created_at timestamp with time zone NOT NULL,
    reporter_contact text,
    body text NOT NULL,
    context_url text,
    resolved_at timestamp with time zone,
    resolved_by uuid,
    CONSTRAINT abuse_reports_body_check CHECK (((char_length(body) >= 1) AND (char_length(body) <= 4000))),
    CONSTRAINT abuse_reports_context_url_check CHECK ((char_length(context_url) <= 2048)),
    CONSTRAINT abuse_reports_reporter_contact_check CHECK ((char_length(reporter_contact) <= 254))
);


--
-- Name: account_consents; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.account_consents (
    id uuid NOT NULL,
    account_id uuid NOT NULL,
    terms_version text NOT NULL,
    consented_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: accounts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.accounts (
    id uuid NOT NULL,
    email text NOT NULL,
    email_verified_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    callsign text,
    display_name text,
    location text,
    grid text,
    avatar_url text,
    deleted_at timestamp with time zone,
    disabled_at timestamp with time zone,
    disabled_reason text
);


--
-- Name: audit_log; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.audit_log (
    id uuid NOT NULL,
    occurred_at timestamp with time zone NOT NULL,
    actor_account_id uuid NOT NULL,
    action text NOT NULL,
    target_type text,
    target_id uuid,
    metadata jsonb,
    context_session_id uuid,
    context_definition_id uuid
);


--
-- Name: auth_methods; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.auth_methods (
    id uuid NOT NULL,
    account_id uuid NOT NULL,
    kind text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_used_at timestamp with time zone,
    CONSTRAINT auth_methods_kind_check CHECK ((kind = 'magic-link'::text))
);


--
-- Name: email_change_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.email_change_tokens (
    id uuid NOT NULL,
    account_id uuid NOT NULL,
    new_email text NOT NULL,
    token_hash bytea NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: magic_link_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.magic_link_tokens (
    id uuid NOT NULL,
    email text NOT NULL,
    token_hash bytea NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: net_connections; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_connections (
    id uuid NOT NULL,
    definition_id uuid NOT NULL,
    "position" integer NOT NULL,
    kind text NOT NULL,
    planned_frequency_hz bigint,
    band text,
    mode text,
    repeater_offset_hz bigint,
    tone_mode text,
    tone_value text,
    node text,
    reflector text,
    talkgroup text,
    label text,
    detail text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    network text
);


--
-- Name: net_definition_owners; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_definition_owners (
    net_definition_id uuid NOT NULL,
    account_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: net_definition_roster; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_definition_roster (
    definition_id uuid NOT NULL,
    callsign text NOT NULL,
    name text,
    location text,
    last_seen_at timestamp with time zone NOT NULL,
    check_in_count bigint DEFAULT 0 NOT NULL
);


--
-- Name: net_definitions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_definitions (
    id uuid NOT NULL,
    definition_version integer DEFAULT 1 NOT NULL,
    title text NOT NULL,
    description text,
    country text,
    state text,
    grid text,
    net_category text NOT NULL,
    net_type text NOT NULL,
    expected_duration_minutes integer,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    visibility text DEFAULT 'listed'::text NOT NULL,
    link_token text NOT NULL,
    archived_at timestamp with time zone
);


--
-- Name: net_delivery_configs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_delivery_configs (
    definition_id uuid NOT NULL,
    delivery_emails text[] DEFAULT '{}'::text[] NOT NULL,
    webhook_url text,
    webhook_secret text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    discord_webhook_url text
);


--
-- Name: net_delivery_jobs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_delivery_jobs (
    id uuid NOT NULL,
    session_id uuid NOT NULL,
    destination text NOT NULL,
    target text DEFAULT ''::text NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    next_attempt_at timestamp with time zone NOT NULL,
    claimed_until timestamp with time zone,
    created_at timestamp with time zone NOT NULL,
    completed_at timestamp with time zone,
    request_sent_at timestamp with time zone,
    CONSTRAINT net_delivery_jobs_state_known CHECK ((state = ANY (ARRAY['pending'::text, 'succeeded'::text, 'failed'::text, 'skipped'::text])))
);


--
-- Name: net_favorites; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_favorites (
    account_id uuid NOT NULL,
    net_definition_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: net_occurrences; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_occurrences (
    id uuid NOT NULL,
    definition_id uuid NOT NULL,
    scheduled_start_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: net_schedules; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_schedules (
    definition_id uuid NOT NULL,
    kind text NOT NULL,
    timezone text NOT NULL,
    one_off_start_at timestamp with time zone,
    frequency text,
    local_hour smallint,
    local_minute smallint,
    weekday smallint,
    day_of_month smallint,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: net_session_roles; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_session_roles (
    net_session_id uuid NOT NULL,
    account_id uuid NOT NULL,
    -- Deliberately un-CHECKed: the role taxonomy is validated in the domain
    -- layer, so adding a role does not cost a migration.
    role text NOT NULL,
    granted_by uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: net_sessions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.net_sessions (
    id uuid NOT NULL,
    definition_id uuid NOT NULL,
    definition_version integer NOT NULL,
    definition_snapshot jsonb NOT NULL,
    lifecycle text NOT NULL,
    started_at timestamp with time zone,
    closed_at timestamp with time zone,
    last_seq bigint DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    control_state text DEFAULT 'active'::text NOT NULL,
    active_ncs_account_id uuid,
    stalled_at_millis bigint
);


--
-- Name: qrz_credentials; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.qrz_credentials (
    account_id uuid NOT NULL,
    kek_version smallint DEFAULT 1 NOT NULL,
    wrapped_dek bytea NOT NULL,
    dek_nonce bytea NOT NULL,
    credential_ciphertext bytea NOT NULL,
    credential_nonce bytea NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: session_events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.session_events (
    id uuid NOT NULL,
    session_id uuid NOT NULL,
    seq bigint NOT NULL,
    kind text NOT NULL,
    payload jsonb NOT NULL,
    -- Deliberately NOT a foreign key to accounts: an account's deletion must
    -- neither be blocked by, nor rewrite, the historical events it caused.
    actor uuid,
    created_at timestamp with time zone NOT NULL
);


--
-- Name: sessions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.sessions (
    id uuid NOT NULL,
    account_id uuid NOT NULL,
    token_hash bytea NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_seen_at timestamp with time zone NOT NULL,
    absolute_expires_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone
);


--
-- Name: abuse_reports abuse_reports_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_reports
    ADD CONSTRAINT abuse_reports_pkey PRIMARY KEY (id);


--
-- Name: account_consents account_consents_account_id_terms_version_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.account_consents
    ADD CONSTRAINT account_consents_account_id_terms_version_key UNIQUE (account_id, terms_version);


--
-- Name: account_consents account_consents_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.account_consents
    ADD CONSTRAINT account_consents_pkey PRIMARY KEY (id);


--
-- Name: accounts accounts_email_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.accounts
    ADD CONSTRAINT accounts_email_key UNIQUE (email);


--
-- Name: accounts accounts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.accounts
    ADD CONSTRAINT accounts_pkey PRIMARY KEY (id);


--
-- Name: audit_log audit_log_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_log
    ADD CONSTRAINT audit_log_pkey PRIMARY KEY (id);


--
-- Name: auth_methods auth_methods_account_id_kind_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.auth_methods
    ADD CONSTRAINT auth_methods_account_id_kind_key UNIQUE (account_id, kind);


--
-- Name: auth_methods auth_methods_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.auth_methods
    ADD CONSTRAINT auth_methods_pkey PRIMARY KEY (id);


--
-- Name: email_change_tokens email_change_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_change_tokens
    ADD CONSTRAINT email_change_tokens_pkey PRIMARY KEY (id);


--
-- Name: email_change_tokens email_change_tokens_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_change_tokens
    ADD CONSTRAINT email_change_tokens_token_hash_key UNIQUE (token_hash);


--
-- Name: magic_link_tokens magic_link_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.magic_link_tokens
    ADD CONSTRAINT magic_link_tokens_pkey PRIMARY KEY (id);


--
-- Name: magic_link_tokens magic_link_tokens_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.magic_link_tokens
    ADD CONSTRAINT magic_link_tokens_token_hash_key UNIQUE (token_hash);


--
-- Name: net_connections net_connections_definition_id_position_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_connections
    ADD CONSTRAINT net_connections_definition_id_position_key UNIQUE (definition_id, "position");


--
-- Name: net_connections net_connections_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_connections
    ADD CONSTRAINT net_connections_pkey PRIMARY KEY (id);


--
-- Name: net_definition_owners net_definition_owners_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_definition_owners
    ADD CONSTRAINT net_definition_owners_pkey PRIMARY KEY (net_definition_id, account_id);


--
-- Name: net_definition_roster net_definition_roster_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_definition_roster
    ADD CONSTRAINT net_definition_roster_pkey PRIMARY KEY (definition_id, callsign);


--
-- Name: net_definitions net_definitions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_definitions
    ADD CONSTRAINT net_definitions_pkey PRIMARY KEY (id);


--
-- Name: net_delivery_configs net_delivery_configs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_delivery_configs
    ADD CONSTRAINT net_delivery_configs_pkey PRIMARY KEY (definition_id);


--
-- Name: net_delivery_jobs net_delivery_jobs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_delivery_jobs
    ADD CONSTRAINT net_delivery_jobs_pkey PRIMARY KEY (id);


--
-- Name: net_delivery_jobs net_delivery_jobs_session_id_destination_target_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_delivery_jobs
    ADD CONSTRAINT net_delivery_jobs_session_id_destination_target_key UNIQUE (session_id, destination, target);


--
-- Name: net_favorites net_favorites_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_favorites
    ADD CONSTRAINT net_favorites_pkey PRIMARY KEY (account_id, net_definition_id);


--
-- Name: net_occurrences net_occurrences_definition_id_scheduled_start_at_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_occurrences
    ADD CONSTRAINT net_occurrences_definition_id_scheduled_start_at_key UNIQUE (definition_id, scheduled_start_at);


--
-- Name: net_occurrences net_occurrences_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_occurrences
    ADD CONSTRAINT net_occurrences_pkey PRIMARY KEY (id);


--
-- Name: net_schedules net_schedules_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_schedules
    ADD CONSTRAINT net_schedules_pkey PRIMARY KEY (definition_id);


--
-- Name: net_session_roles net_session_roles_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_session_roles
    ADD CONSTRAINT net_session_roles_pkey PRIMARY KEY (net_session_id, account_id);


--
-- Name: net_sessions net_sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_sessions
    ADD CONSTRAINT net_sessions_pkey PRIMARY KEY (id);


--
-- Name: qrz_credentials qrz_credentials_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.qrz_credentials
    ADD CONSTRAINT qrz_credentials_pkey PRIMARY KEY (account_id);


--
-- Name: session_events session_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.session_events
    ADD CONSTRAINT session_events_pkey PRIMARY KEY (id);


--
-- Name: session_events session_events_session_id_seq_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.session_events
    ADD CONSTRAINT session_events_session_id_seq_key UNIQUE (session_id, seq);


--
-- Name: sessions sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_pkey PRIMARY KEY (id);


--
-- Name: sessions sessions_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_token_hash_key UNIQUE (token_hash);


--
-- Name: idx_abuse_reports_unresolved; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_abuse_reports_unresolved ON public.abuse_reports USING btree (created_at) WHERE (resolved_at IS NULL);


--
-- Name: idx_account_consents_account_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_account_consents_account_id ON public.account_consents USING btree (account_id);


--
-- Name: idx_accounts_callsign; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_accounts_callsign ON public.accounts USING btree (callsign) WHERE (callsign IS NOT NULL);


--
-- Name: idx_accounts_callsign_prefix; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_accounts_callsign_prefix ON public.accounts USING btree (lower(callsign) text_pattern_ops) WHERE (callsign IS NOT NULL);


--
-- Name: idx_accounts_display_name_prefix; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_accounts_display_name_prefix ON public.accounts USING btree (lower(display_name) text_pattern_ops) WHERE (display_name IS NOT NULL);


--
-- Name: idx_accounts_pending_deletion; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_accounts_pending_deletion ON public.accounts USING btree (deleted_at) WHERE (deleted_at IS NOT NULL);


--
-- Name: idx_audit_log_actor_recent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_log_actor_recent ON public.audit_log USING btree (actor_account_id, occurred_at DESC, id DESC);


--
-- Name: idx_audit_log_context_definition_recent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_log_context_definition_recent ON public.audit_log USING btree (context_definition_id, occurred_at DESC, id DESC) WHERE (context_definition_id IS NOT NULL);


--
-- Name: idx_audit_log_context_session_recent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_log_context_session_recent ON public.audit_log USING btree (context_session_id, occurred_at DESC, id DESC) WHERE (context_session_id IS NOT NULL);


--
-- Name: idx_audit_log_occurred_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_log_occurred_at ON public.audit_log USING btree (occurred_at);


--
-- Name: idx_audit_log_target_recent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_log_target_recent ON public.audit_log USING btree (target_id, occurred_at DESC, id DESC) WHERE (target_id IS NOT NULL);


--
-- Name: idx_email_change_tokens_account_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_email_change_tokens_account_id ON public.email_change_tokens USING btree (account_id);


--
-- Name: idx_magic_link_tokens_email; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_magic_link_tokens_email ON public.magic_link_tokens USING btree (email);


--
-- Name: idx_net_connections_export_position; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_net_connections_export_position ON public.net_connections USING btree (definition_id) WHERE ("position" = 0);


--
-- Name: idx_net_definition_owners_account_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_definition_owners_account_id ON public.net_definition_owners USING btree (account_id);


--
-- Name: idx_net_definitions_link_token; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_net_definitions_link_token ON public.net_definitions USING btree (link_token);


--
-- Name: idx_net_definitions_title_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_definitions_title_trgm ON public.net_definitions USING gin (title gin_trgm_ops);


--
-- Name: idx_net_delivery_jobs_due; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_delivery_jobs_due ON public.net_delivery_jobs USING btree (next_attempt_at) WHERE (state = 'pending'::text);


--
-- Name: idx_net_delivery_jobs_prune; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_delivery_jobs_prune ON public.net_delivery_jobs USING btree (completed_at) WHERE (state <> 'pending'::text);


--
-- Name: idx_net_favorites_account_recent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_favorites_account_recent ON public.net_favorites USING btree (account_id, created_at DESC, net_definition_id DESC);


--
-- Name: idx_net_favorites_net_definition_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_favorites_net_definition_id ON public.net_favorites USING btree (net_definition_id);


--
-- Name: idx_net_occurrences_scheduled_start_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_occurrences_scheduled_start_at ON public.net_occurrences USING btree (scheduled_start_at);


--
-- Name: idx_net_session_roles_account_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_session_roles_account_id ON public.net_session_roles USING btree (account_id);


--
-- Name: idx_net_sessions_definition_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_sessions_definition_id ON public.net_sessions USING btree (definition_id);


--
-- Name: idx_net_sessions_snapshot_title_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_net_sessions_snapshot_title_trgm ON public.net_sessions USING gin (((definition_snapshot ->> 'title'::text)) gin_trgm_ops);


--
-- Name: idx_session_events_actor; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_session_events_actor ON public.session_events USING btree (actor) WHERE (actor IS NOT NULL);


--
-- Name: idx_sessions_account_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_sessions_account_id ON public.sessions USING btree (account_id);


--
-- Name: net_sessions_live_control_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX net_sessions_live_control_state ON public.net_sessions USING btree (control_state) WHERE (lifecycle = 'live'::text);


--
-- Name: net_sessions_one_live_per_definition; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX net_sessions_one_live_per_definition ON public.net_sessions USING btree (definition_id) WHERE (lifecycle = 'live'::text);


--
-- Name: session_events_checkin_client_event_id_idempotent; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX session_events_checkin_client_event_id_idempotent ON public.session_events USING btree (session_id, ((payload ->> 'clientEventId'::text))) WHERE ((kind = 'checkin.added'::text) AND (payload ? 'clientEventId'::text));


--
-- Name: account_consents account_consents_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.account_consents
    ADD CONSTRAINT account_consents_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;


--
-- Name: auth_methods auth_methods_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.auth_methods
    ADD CONSTRAINT auth_methods_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;


--
-- Name: email_change_tokens email_change_tokens_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_change_tokens
    ADD CONSTRAINT email_change_tokens_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;


--
-- Name: net_connections net_connections_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_connections
    ADD CONSTRAINT net_connections_definition_id_fkey FOREIGN KEY (definition_id) REFERENCES public.net_definitions(id) ON DELETE CASCADE;


--
-- Name: net_definition_owners net_definition_owners_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_definition_owners
    ADD CONSTRAINT net_definition_owners_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;


--
-- Name: net_definition_owners net_definition_owners_net_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_definition_owners
    ADD CONSTRAINT net_definition_owners_net_definition_id_fkey FOREIGN KEY (net_definition_id) REFERENCES public.net_definitions(id) ON DELETE CASCADE;


--
-- Name: net_definition_roster net_definition_roster_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

-- One of the two foreign keys here with no ON DELETE clause: this projection is
-- rebuildable from session_events, and NO ACTION keeps a definition that has
-- roster memory from being hard-deleted out from under it.
ALTER TABLE ONLY public.net_definition_roster
    ADD CONSTRAINT net_definition_roster_definition_id_fkey FOREIGN KEY (definition_id) REFERENCES public.net_definitions(id);


--
-- Name: net_delivery_configs net_delivery_configs_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_delivery_configs
    ADD CONSTRAINT net_delivery_configs_definition_id_fkey FOREIGN KEY (definition_id) REFERENCES public.net_definitions(id) ON DELETE CASCADE;


--
-- Name: net_delivery_jobs net_delivery_jobs_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_delivery_jobs
    ADD CONSTRAINT net_delivery_jobs_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.net_sessions(id) ON DELETE CASCADE;


--
-- Name: net_favorites net_favorites_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_favorites
    ADD CONSTRAINT net_favorites_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;


--
-- Name: net_favorites net_favorites_net_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_favorites
    ADD CONSTRAINT net_favorites_net_definition_id_fkey FOREIGN KEY (net_definition_id) REFERENCES public.net_definitions(id) ON DELETE CASCADE;


--
-- Name: net_occurrences net_occurrences_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_occurrences
    ADD CONSTRAINT net_occurrences_definition_id_fkey FOREIGN KEY (definition_id) REFERENCES public.net_definitions(id) ON DELETE CASCADE;


--
-- Name: net_schedules net_schedules_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_schedules
    ADD CONSTRAINT net_schedules_definition_id_fkey FOREIGN KEY (definition_id) REFERENCES public.net_definitions(id) ON DELETE CASCADE;


--
-- Name: net_session_roles net_session_roles_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_session_roles
    ADD CONSTRAINT net_session_roles_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;


--
-- Name: net_session_roles net_session_roles_granted_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_session_roles
    ADD CONSTRAINT net_session_roles_granted_by_fkey FOREIGN KEY (granted_by) REFERENCES public.accounts(id) ON DELETE SET NULL;


--
-- Name: net_session_roles net_session_roles_net_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_session_roles
    ADD CONSTRAINT net_session_roles_net_session_id_fkey FOREIGN KEY (net_session_id) REFERENCES public.net_sessions(id) ON DELETE CASCADE;


--
-- Name: net_sessions net_sessions_active_ncs_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.net_sessions
    ADD CONSTRAINT net_sessions_active_ncs_account_id_fkey FOREIGN KEY (active_ncs_account_id) REFERENCES public.accounts(id) ON DELETE SET NULL;


--
-- Name: net_sessions net_sessions_definition_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

-- One of the two foreign keys here with no ON DELETE clause: a started session
-- outlives its definition, so deleting a definition must never orphan or
-- remove session history.
ALTER TABLE ONLY public.net_sessions
    ADD CONSTRAINT net_sessions_definition_id_fkey FOREIGN KEY (definition_id) REFERENCES public.net_definitions(id);


--
-- Name: qrz_credentials qrz_credentials_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.qrz_credentials
    ADD CONSTRAINT qrz_credentials_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;


--
-- Name: session_events session_events_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.session_events
    ADD CONSTRAINT session_events_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.net_sessions(id) ON DELETE CASCADE;


--
-- Name: sessions sessions_account_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_account_id_fkey FOREIGN KEY (account_id) REFERENCES public.accounts(id) ON DELETE CASCADE;

