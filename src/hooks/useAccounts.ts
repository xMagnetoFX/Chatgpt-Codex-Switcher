import { useState, useEffect, useCallback, useRef } from "react";
import type {
  AccountInfo,
  UsageInfo,
  AccountWithUsage,
  WarmupSummary,
  ImportAccountsSummary,
} from "../types";
import { invokeBackend, type FileSource } from "../lib/platform";

function resolvePlanType(usage: UsageInfo, fallback: string | null): string | null {
  if (usage.error) return fallback;
  const livePlanType = usage.plan_type?.trim();
  return livePlanType || fallback;
}

interface RefreshUsageOptions {
  refreshMetadata?: boolean;
}

export function useAccounts() {
  const [accounts, setAccounts] = useState<AccountWithUsage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const accountsRef = useRef<AccountWithUsage[]>([]);
  const loadRequestIdRef = useRef(0);
  const maxConcurrentUsageRequests = 10;

  useEffect(() => {
    accountsRef.current = accounts;
  }, [accounts]);

  const buildUsageError = useCallback(
    (accountId: string, message: string, planType: string | null): UsageInfo => ({
      account_id: accountId,
      plan_type: planType,
      primary_used_percent: null,
      primary_window_minutes: null,
      primary_resets_at: null,
      secondary_used_percent: null,
      secondary_window_minutes: null,
      secondary_resets_at: null,
      has_credits: null,
      unlimited_credits: null,
      credits_balance: null,
      banked_resets: null,
      error: message,
    }),
    []
  );

  const runWithConcurrency = useCallback(
    async <T>(items: T[], worker: (item: T) => Promise<void>, concurrency: number) => {
      if (items.length === 0) return;
      const limit = Math.min(Math.max(concurrency, 1), items.length);
      let index = 0;
      const runners = Array.from({ length: limit }, async () => {
        while (true) {
          const current = index++;
          if (current >= items.length) return;
          await worker(items[current]);
        }
      });
      await Promise.allSettled(runners);
    },
    []
  );

  const loadAccounts = useCallback(async (preserveUsage = false) => {
    const requestId = ++loadRequestIdRef.current;
    setLoading(true);
    setError(null);

    try {
      const accountList = await invokeBackend<AccountInfo[]>("list_accounts");
      if (requestId !== loadRequestIdRef.current) return accountList;

      if (preserveUsage) {
        // Preserve existing usage data when just updating account info
        setAccounts((prev) => {
          const usageMap = new Map(
            prev.map((a) => [a.id, { usage: a.usage, usageLoading: a.usageLoading }])
          );
          return accountList.map((a) => ({
            ...a,
            usage: usageMap.get(a.id)?.usage,
            usageLoading: usageMap.get(a.id)?.usageLoading,
          }));
        });
      } else {
        setAccounts(accountList.map((a) => ({ ...a, usageLoading: false })));
      }
      return accountList;
    } catch (err) {
      if (requestId === loadRequestIdRef.current) {
        setError(err instanceof Error ? err.message : String(err));
      }
      throw err;
    } finally {
      if (requestId === loadRequestIdRef.current) {
        setLoading(false);
      }
    }
  }, []);

  const refreshAccountMetadata = useCallback(
    async (accountList: AccountInfo[] | AccountWithUsage[]) => {
      const chatGptAccounts = accountList.filter((account) => account.auth_mode === "chat_gpt");
      if (chatGptAccounts.length === 0) return accountList;

      await runWithConcurrency(
        chatGptAccounts,
        async (account) => {
          try {
            await invokeBackend<AccountInfo>("refresh_account_metadata", {
              accountId: account.id,
            });
          } catch (err) {
            console.warn("Failed to refresh optional subscription metadata:", err);
          }
        },
        maxConcurrentUsageRequests
      );

      return loadAccounts(true);
    },
    [loadAccounts, maxConcurrentUsageRequests, runWithConcurrency]
  );

  const refreshUsage = useCallback(
    async (accountList?: AccountInfo[] | AccountWithUsage[], options?: RefreshUsageOptions) => {
      try {
        let list = accountList ?? accountsRef.current;
        if (list.length === 0) {
          return;
        }

        if (options?.refreshMetadata) {
          list = await refreshAccountMetadata(list);
        }

        const accountIds = list.map((account) => account.id);
        const accountIdSet = new Set(accountIds);
        const usageResults = new Map<string, UsageInfo>();

        setAccounts((prev) =>
          prev.map((account) =>
            accountIdSet.has(account.id) ? { ...account, usageLoading: true } : account
          )
        );

        await runWithConcurrency(
          list,
          async (account) => {
            try {
              const usage = await invokeBackend<UsageInfo>("get_usage", {
                accountId: account.id,
              });
              usageResults.set(account.id, usage);
            } catch (err) {
              console.error("Failed to refresh usage:", err);
              const message = err instanceof Error ? err.message : String(err);
              usageResults.set(
                account.id,
                buildUsageError(account.id, message, account.plan_type ?? null)
              );
            }
          },
          maxConcurrentUsageRequests
        );

        setAccounts((prev) =>
          prev.map((account) => {
            const usage = usageResults.get(account.id);
            if (!usage) return account;
            return {
              ...account,
              plan_type: resolvePlanType(usage, account.plan_type),
              usage,
              usageLoading: false,
            };
          })
        );
      } catch (err) {
        console.error("Failed to refresh usage:", err);
        throw err;
      }
    },
    [buildUsageError, maxConcurrentUsageRequests, refreshAccountMetadata, runWithConcurrency]
  );

  const reloadAfterMutation = useCallback(
    async (preserveUsage = false, refreshLoadedUsage = false) => {
      try {
        const accountList = await loadAccounts(preserveUsage);
        if (refreshLoadedUsage) {
          await refreshUsage(accountList, { refreshMetadata: true });
        }
      } catch (err) {
        console.error("The account change succeeded, but reloading the account list failed:", err);
      }
    },
    [loadAccounts, refreshUsage]
  );

  const refreshSingleUsage = useCallback(async (accountId: string) => {
    try {
      setAccounts((prev) =>
        prev.map((a) => (a.id === accountId ? { ...a, usageLoading: true } : a))
      );
      const usage = await invokeBackend<UsageInfo>("get_usage", { accountId });
      setAccounts((prev) =>
        prev.map((a) =>
          a.id === accountId
            ? {
                ...a,
                plan_type: resolvePlanType(usage, a.plan_type),
                usage,
                usageLoading: false,
              }
            : a
        )
      );
    } catch (err) {
      console.error("Failed to refresh single usage:", err);
      const message = err instanceof Error ? err.message : String(err);
      setAccounts((prev) =>
        prev.map((a) =>
          a.id === accountId
            ? {
                ...a,
                usage: buildUsageError(accountId, message, a.plan_type ?? null),
                usageLoading: false,
              }
            : a
        )
      );
      throw err;
    }
  }, [buildUsageError]);

  const warmupAccount = useCallback(async (accountId: string) => {
    try {
      await invokeBackend("warmup_account", { accountId });
    } catch (err) {
      console.error("Failed to warm up account:", err);
      throw err;
    }
  }, []);

  const warmupAllAccounts = useCallback(async () => {
    try {
      return await invokeBackend<WarmupSummary>("warmup_all_accounts");
    } catch (err) {
      console.error("Failed to warm up all accounts:", err);
      throw err;
    }
  }, []);

  const switchAccount = useCallback(
    async (accountId: string) => {
      try {
        await invokeBackend("switch_account", { accountId });
      } catch (err) {
        await reloadAfterMutation(true);
        throw err;
      }
      await reloadAfterMutation(true);
    },
    [reloadAfterMutation]
  );

  const restartCodexAndSwitchAccount = useCallback(
    async (accountId: string) => {
      try {
        await invokeBackend("restart_codex_and_switch_account", { accountId });
      } catch (err) {
        await reloadAfterMutation(true);
        throw err;
      }
      await reloadAfterMutation(true);
    },
    [reloadAfterMutation]
  );

  const deleteAccount = useCallback(
    async (accountId: string) => {
      try {
        await invokeBackend("delete_account", { accountId });
      } catch (err) {
        await reloadAfterMutation();
        throw err;
      }
      await reloadAfterMutation();
    },
    [reloadAfterMutation]
  );

  const renameAccount = useCallback(
    async (accountId: string, newName: string) => {
      try {
        await invokeBackend("rename_account", { accountId, newName });
      } catch (err) {
        await reloadAfterMutation(true);
        throw err;
      }
      await reloadAfterMutation(true);
    },
    [reloadAfterMutation]
  );

  const importFromFile = useCallback(
    async (source: FileSource) => {
      try {
        if (typeof source === "string") {
          await invokeBackend<AccountInfo>("add_account_from_file", { path: source });
        } else {
          const contents = await source.text();
          await invokeBackend<AccountInfo>("add_account_from_auth_json_text", {
            contents,
          });
        }
      } catch (err) {
        await reloadAfterMutation(false, true);
        throw err;
      }
      await reloadAfterMutation(false, true);
    },
    [reloadAfterMutation]
  );

  const startOAuthLogin = useCallback(async () => {
    try {
      const info = await invokeBackend<{ auth_url: string; callback_port: number }>("start_login");
      return info;
    } catch (err) {
      throw err;
    }
  }, []);

  const completeOAuthLogin = useCallback(async () => {
    let account: AccountInfo;
    try {
      account = await invokeBackend<AccountInfo>("complete_login");
    } catch (err) {
      await reloadAfterMutation(false, true);
      throw err;
    }
    await reloadAfterMutation(false, true);
    return account;
  }, [reloadAfterMutation]);

  const exportAccountsSlimText = useCallback(async () => {
    try {
      return await invokeBackend<string>("export_accounts_slim_text");
    } catch (err) {
      throw err;
    }
  }, []);

  const importAccountsSlimText = useCallback(
    async (payload: string) => {
      let summary: ImportAccountsSummary;
      try {
        summary = await invokeBackend<ImportAccountsSummary>("import_accounts_slim_text", {
          payload,
        });
      } catch (err) {
        await reloadAfterMutation(false, true);
        throw err;
      }
      await reloadAfterMutation(false, true);
      return summary;
    },
    [reloadAfterMutation]
  );

  const exportAccountsFullEncryptedFile = useCallback(async (path: string) => {
    try {
      await invokeBackend("export_accounts_full_encrypted_file", { path });
    } catch (err) {
      throw err;
    }
  }, []);

  const importAccountsFullEncryptedFile = useCallback(
    async (path: string) => {
      let summary: ImportAccountsSummary;
      try {
        summary = await invokeBackend<ImportAccountsSummary>(
          "import_accounts_full_encrypted_file",
          { path }
        );
      } catch (err) {
        await reloadAfterMutation(false, true);
        throw err;
      }
      await reloadAfterMutation(false, true);
      return summary;
    },
    [reloadAfterMutation]
  );

  const cancelOAuthLogin = useCallback(async () => {
    try {
      await invokeBackend("cancel_login");
    } catch (err) {
      console.error("Failed to cancel login:", err);
    }
  }, []);

  const loadMaskedAccountIds = useCallback(async () => {
    try {
      return await invokeBackend<string[]>("get_masked_account_ids");
    } catch (err) {
      console.error("Failed to load masked account IDs:", err);
      return [];
    }
  }, []);

  const saveMaskedAccountIds = useCallback(async (ids: string[]) => {
    try {
      await invokeBackend("set_masked_account_ids", { ids });
    } catch (err) {
      console.error("Failed to save masked account IDs:", err);
    }
  }, []);

  useEffect(() => {
    loadAccounts()
      .then((accountList) => refreshUsage(accountList, { refreshMetadata: true }))
      .catch((err) => {
        console.error("Failed to load initial usage:", err);
      });

    // Auto-refresh usage every 60 seconds (same as official Codex CLI)
    const interval = setInterval(() => {
      refreshUsage().catch(() => {});
    }, 60000);

    return () => clearInterval(interval);
  }, [loadAccounts, refreshUsage]);

  return {
    accounts,
    loading,
    error,
    loadAccounts,
    refreshUsage,
    refreshSingleUsage,
    warmupAccount,
    warmupAllAccounts,
    switchAccount,
    restartCodexAndSwitchAccount,
    deleteAccount,
    renameAccount,
    importFromFile,
    exportAccountsSlimText,
    importAccountsSlimText,
    exportAccountsFullEncryptedFile,
    importAccountsFullEncryptedFile,
    startOAuthLogin,
    completeOAuthLogin,
    cancelOAuthLogin,
    loadMaskedAccountIds,
    saveMaskedAccountIds,
  };
}
