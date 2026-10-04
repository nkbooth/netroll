// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";
import { useStore } from "zustand";
import type { StoreApi } from "zustand/vanilla";

import { viaLabel } from "../nets/connectionPresentation";
import type { NetConnection } from "../nets/netsApi";
import { ViaPicker } from "./ViaPicker";
import type { SessionStore } from "./sessionStore";
import { tokens } from "../../ui/tokens/tokens";

/**
 * "Taking check-ins on X now" — the session-level way-in
 * stamp, which every subsequent quick-add sends as its `via` at no hot-path
 * cost at all: the block case costs the keystrokes it costs today and not one
 * more.
 *
 * The stamp is PER OPERATOR and IN MEMORY (see {@link SessionStore.viaStamp}
 * for both rejected alternatives), so it does not survive a
 * reload. That is only acceptable because of the line this control always
 * renders: the operator never has to answer "what is this recording?" from
 * memory, and after a reload it says, truthfully, that nothing is stamped.
 *
 * It writes NOTHING to the server. The quick-add reads the stamp from the same
 * store, so nothing is passed down from the page that mounts this.
 */

const containerStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
  minWidth: 0,
};

const statusStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

const setStyle: CSSProperties = { ...statusStyle, color: "var(--text)", fontWeight: 700 };

/** What the stamp control calls "no stamp" — the quick-add says it differently,
 * because there the same state means "whatever the stamp says", not "nothing". */
const NOT_STAMPED_LABEL = "Not stamped — records no way in";

export interface ViaStampControlProps {
  /** The bound session store — the stamp's only home. */
  readonly store: StoreApi<SessionStore>;
  /** The session's frozen connection set, in the owner's order. */
  readonly connections: readonly NetConnection[];
}

/** The toolbar's "taking check-ins on" stamp. */
export function ViaStampControl({ store, connections }: ViaStampControlProps): ReactElement {
  const stamp = useStore(store, (s) => s.viaStamp);
  const setViaStamp = useStore(store, (s) => s.setViaStamp);
  const label = viaLabel(stamp, connections);

  return (
    <div style={containerStyle}>
      <ViaPicker
        label="Taking check-ins on"
        unsetLabel={NOT_STAMPED_LABEL}
        connections={connections}
        value={stamp}
        onChange={setViaStamp}
      />
      {/* Stated ALWAYS, not only once something is set: "nothing is stamped" is
          the answer an operator most needs after a reload, and it is the one a
          control that renders only when it has news would never give. */}
      <span
        role="status"
        aria-label="Taking check-ins on"
        data-via-stamp={label === null ? "none" : "set"}
        style={label === null ? statusStyle : setStyle}
      >
        {label === null ? NOT_STAMPED_LABEL : `Taking check-ins on ${label}`}
      </span>
    </div>
  );
}
