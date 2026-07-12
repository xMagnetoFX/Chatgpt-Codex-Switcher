import type { ConfigModalMode } from "../types/ui";
import { ArchiveDownIcon, ArchiveUpIcon, CloseIcon } from "./icons";

interface ConfigModalProps {
  mode: ConfigModalMode;
  payload: string;
  error: string | null;
  copied: boolean;
  isExportingSlim: boolean;
  isImportingSlim: boolean;
  onPayloadChange: (payload: string) => void;
  onClose: () => void;
  onCopy: () => void;
  onImport: () => void;
}

export function ConfigModal({
  mode,
  payload,
  error,
  copied,
  isExportingSlim,
  isImportingSlim,
  onPayloadChange,
  onClose,
  onCopy,
  onImport,
}: ConfigModalProps) {
  return (
    <div className="modal-backdrop">
      <div className="modal" style={{ maxWidth: 560 }}>
        {/* Head */}
        <div className="modal-head">
          <div>
            <div className="kicker">Slim transfer</div>
            <h3 className="modal-title">
              {mode === "slim_export" ? "Export slim text" : "Import slim text"}
            </h3>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="icon-btn flex-shrink-0"
            title="Close"
            aria-label="Close slim transfer modal"
          >
            <CloseIcon className="h-4 w-4" />
          </button>
        </div>

        {/* Body */}
        <div className="modal-body">
          {mode === "slim_import" ? (
            <div
              style={{
                padding: "10px 14px",
                borderRadius: 12,
                border: "1px solid var(--warning-border)",
                background: "var(--warning-soft)",
                fontSize: 12.5,
                color: "var(--warning)",
                lineHeight: 1.5,
              }}
            >
              Existing accounts stay in place. Only missing accounts are imported.
            </div>
          ) : (
            <div
              style={{
                padding: "10px 14px",
                borderRadius: 12,
                border: "1px solid var(--hairline)",
                background: "var(--glass-1)",
                fontSize: 12.5,
                color: "var(--text-muted)",
                lineHeight: 1.5,
              }}
            >
              This slim string contains account secrets. Keep it private and share it only on a channel you trust.
            </div>
          )}

          <textarea
            value={payload}
            onChange={(event) => onPayloadChange(event.target.value)}
            readOnly={mode === "slim_export"}
            placeholder={
              mode === "slim_export"
                ? isExportingSlim
                  ? "Generating your slim text payload…"
                  : "The export string will appear here."
                : "Paste the slim text payload here."
            }
            className="textarea-shell"
          />

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
          <button type="button" onClick={onClose} className="btn-ghost">
            Close
          </button>
          {mode === "slim_export" ? (
            <button
              type="button"
              onClick={onCopy}
              disabled={!payload || isExportingSlim}
              className="btn-primary flex-1"
            >
              <ArchiveUpIcon className="h-4 w-4" />
              {copied ? "Copied" : "Copy string"}
            </button>
          ) : (
            <button
              type="button"
              onClick={onImport}
              disabled={isImportingSlim}
              className="btn-primary flex-1"
            >
              <ArchiveDownIcon className="h-4 w-4" />
              {isImportingSlim ? "Importing…" : "Import missing accounts"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
