// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useState } from "react";

import { deleteCurrentSession, fetchCurrentAccount } from "./authApi";
import type { Account } from "./authApi";

/**
 * The shell's session awareness: loads `/api/accounts/me` on mount and
 * exposes sign-out. A failed session check reads as signed-out — the shell
 * never blocks on auth — but `loading` is surfaced so the shell can avoid
 * flashing "Sign in" at a user whose session is still being checked.
 */
export function useCurrentAccount(): {
  account: Account | null;
  loading: boolean;
  signOutFailed: boolean;
  signOut: () => Promise<void>;
} {
  const [account, setAccount] = useState<Account | null>(null);
  const [loading, setLoading] = useState(true);
  const [signOutFailed, setSignOutFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetchCurrentAccount()
      .then((current) => {
        if (!cancelled) {
          setAccount(current);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setAccount(null);
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const signOut = useCallback(async () => {
    setSignOutFailed(false);
    try {
      await deleteCurrentSession();
      setAccount(null);
    } catch {
      // The server session was never revoked and the cookie is still live;
      // clearing local state would show a signed-out UI the next reload
      // silently contradicts. Stay signed in and say so.
      setSignOutFailed(true);
    }
  }, []);

  return { account, loading, signOutFailed, signOut };
}
