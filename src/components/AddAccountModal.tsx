import { useRef, useState } from "react";
import {
  describeFileSource,
  isTauriRuntime,
  openExternalUrl,
  pickAuthJsonFile,
  type FileSource,
} from "../lib/platform";
import { CloseIcon, CopyIcon, ExternalIcon, FolderIcon, RefreshIcon } from "./icons";

interface AddAccountModalProps {
  isOpen: boolean;
  onClose: () => void;
  onImportFile: (source: FileSource) => Promise<void>;
  onStartOAuth: () => Promise<{ auth_url: string }>;
  onCompleteOAuth: () => Promise<unknown>;
  onCancelOAuth: () => Promise<void>;
}

type Tab = "oauth" | "import";

export function AddAccountModal({
  isOpen,
  onClose,
  onImportFile,
  onStartOAuth,
  onCompleteOAuth,
  onCancelOAuth,
}: AddAccountModalProps) {
  const [activeTab, setActiveTab] = useState<Tab>("oauth");
  const [fileSource, setFileSource] = useState<FileSource | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [oauthPending, setOauthPending] = useState(false);
  const [authUrl, setAuthUrl] = useState<string>("");
  const [copied, setCopied] = useState<boolean>(false);
  const isPrimaryDisabled = loading || (activeTab === "oauth" && oauthPending);
  const oauthInFlight = activeTab === "oauth" && (loading || oauthPending);
  const tauriRuntime = isTauriRuntime();
  // Bumped whenever a flow is superseded (close, cancel, retry) so an
  // in-flight completeOAuth rejection can't write stale error state later.
  const oauthAttemptRef = useRef(0);

  const resetForm = () => {
    oauthAttemptRef.current += 1;
    setFileSource(null);
    setError(null);
    setLoading(false);
    setOauthPending(false);
    setAuthUrl("");
    setCopied(false);
  };

  const handleClose = () => {
    if (oauthInFlight) void onCancelOAuth();
    resetForm();
    onClose();
  };

  const handleOAuthLogin = async () => {
    const attempt = ++oauthAttemptRef.current;
    try {
      setLoading(true);
      setError(null);
      const info = await onStartOAuth();
      if (attempt !== oauthAttemptRef.current) return;
      setAuthUrl(info.auth_url);
      setOauthPending(true);
      setLoading(false);
      await onCompleteOAuth();
      if (attempt !== oauthAttemptRef.current) return;
      handleClose();
    } catch (err) {
      if (attempt !== oauthAttemptRef.current) return;
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
      setOauthPending(false);
    }
  };

  const handleSelectFile = async () => {
    try {
      const selected = await pickAuthJsonFile();
      if (selected) setFileSource(selected);
    } catch (err) {
      console.error("Failed to open file dialog:", err);
    }
  };

  const handleImportFile = async () => {
    if (!fileSource) { setError("Please select an auth.json file"); return; }
    try {
      setLoading(true);
      setError(null);
      await onImportFile(fileSource);
      handleClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
    }
  };

  if (!isOpen) return null;

  return (
    <div className="modal-backdrop">
      <div className="modal">
        {/* Head */}
        <div className="modal-head">
          <div>
            <div className="kicker">Account intake</div>
            <h3 className="modal-title">Add account</h3>
            <p className="modal-sub">Sign in with OAuth or import an existing auth.json file.</p>
          </div>
          <button
            type="button"
            onClick={handleClose}
            className="icon-btn flex-shrink-0"
            title="Close"
            aria-label="Close"
          >
            <CloseIcon className="h-4 w-4" />
          </button>
        </div>

        {/* Tabs */}
        <div className="modal-tabs">
          {(["oauth", "import"] as Tab[]).map((tab) => (
            <button
              type="button"
              key={tab}
              onClick={() => {
                if (tab === "import" && oauthInFlight) {
                  oauthAttemptRef.current += 1;
                  void onCancelOAuth().catch((err) => console.error("Failed to cancel login:", err));
                  setOauthPending(false);
                  setLoading(false);
                }
                setActiveTab(tab);
                setError(null);
              }}
              className={`modal-tab ${activeTab === tab ? "active" : ""}`}
            >
              {tab === "oauth" ? "ChatGPT login" : "Import file"}
            </button>
          ))}
        </div>

        {/* Body */}
        <div className="modal-body">
          {activeTab === "oauth" && (
            <div>
              {oauthPending ? (
                <div className="flex flex-col gap-4">
                  <div className="flex items-center gap-3">
                    <RefreshIcon className="h-5 w-5 animate-spin" style={{ color: "var(--accent)" }} />
                    <div>
                      <div style={{ fontSize: 13.5, fontWeight: 500, color: "var(--text-strong)" }}>
                        Waiting for browser login
                      </div>
                      <p style={{ margin: "3px 0 0", fontSize: 12, color: "var(--text-muted)", lineHeight: 1.5 }}>
                        Open the generated link in your browser, then come back once the callback lands on localhost.
                      </p>
                    </div>
                  </div>

                  <div
                    style={{
                      padding: "12px 14px",
                      borderRadius: 12,
                      border: "1px solid var(--hairline)",
                      background: "var(--glass-1)",
                    }}
                  >
                    <div className="field-label">Login URL</div>
                    <div style={{ fontSize: 11.5, color: "var(--text-muted)", wordBreak: "break-all", lineHeight: 1.5 }}>
                      {authUrl}
                    </div>
                  </div>

                  <div className="flex gap-3 flex-col sm:flex-row">
                    <button
                      type="button"
                      onClick={() => {
                        void navigator.clipboard.writeText(authUrl)
                          .then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 2000); })
                          .catch(() => setError("Clipboard unavailable. Copy the link manually."));
                      }}
                      className="btn-ghost flex-1 justify-center"
                    >
                      <CopyIcon className="h-4 w-4" />
                      {copied ? "Copied" : "Copy link"}
                    </button>
                    <button
                      type="button"
                      onClick={() => void openExternalUrl(authUrl)}
                      className="btn-primary flex-1 justify-center"
                    >
                      <ExternalIcon className="h-4 w-4" />
                      Open in browser
                    </button>
                  </div>

                  {!tauriRuntime && (
                    <p style={{ fontSize: 11.5, color: "var(--warning)", lineHeight: 1.5 }}>
                      OAuth login must finish on the same host machine because the callback redirects to `localhost`.
                    </p>
                  )}
                </div>
              ) : (
                <div>
                  <div className="kicker-faint" style={{ marginBottom: 6 }}>Browser flow</div>
                  <p style={{ margin: 0, fontSize: 13, color: "var(--text-muted)", lineHeight: 1.55 }}>
                    Generate a login link, finish the authentication in your browser, and the account will return to this desktop app automatically.
                  </p>
                </div>
              )}
            </div>
          )}

          {activeTab === "import" && (
            <div className="flex flex-col gap-3">
              <label className="field-label" style={{ marginBottom: 0 }}>
                Select auth.json file
              </label>
              <div className="flex gap-3 items-stretch">
                <div
                  className="field-input flex-1 truncate flex items-center"
                  style={{ cursor: "default" }}
                >
                  {describeFileSource(fileSource)}
                </div>
                <button
                  type="button"
                  onClick={handleSelectFile}
                  className="btn-ghost flex-shrink-0"
                >
                  <FolderIcon className="h-4 w-4" />
                  Browse
                </button>
              </div>
              <p style={{ margin: 0, fontSize: 11.5, color: "var(--text-faint)", lineHeight: 1.5 }}>
                Import credentials from an existing Codex `auth.json` file.
              </p>
            </div>
          )}

          {error && (
            <div
              style={{
                padding: "10px 14px",
                borderRadius: 12,
                border: "1px solid var(--danger-border)",
                background: "var(--danger-soft)",
                fontSize: 12.5,
                color: "var(--danger)",
                lineHeight: 1.5,
              }}
            >
              {error}
            </div>
          )}
        </div>

        {/* Foot */}
        <div className="modal-foot">
          <button type="button" onClick={handleClose} className="btn-ghost">
            Cancel
          </button>
          <button
            type="button"
            onClick={activeTab === "oauth" ? () => void handleOAuthLogin() : () => void handleImportFile()}
            disabled={isPrimaryDisabled}
            className="btn-primary flex-1"
          >
            {activeTab === "oauth" ? (
              <>
                <ExternalIcon className="h-4 w-4" />
                {loading ? "Generating…" : "Generate login link"}
              </>
            ) : (
              <>
                <FolderIcon className="h-4 w-4" />
                {loading ? "Importing…" : "Import account"}
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
