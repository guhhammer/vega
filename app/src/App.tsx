import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  MAX_FILE_BYTES,
  onMessage,
  onNetwork,
  toBase64,
  type Contact,
  type History,
  type Me,
  type Message,
} from "./api";

type Sheet = "none" | "invite" | "add" | "history";

/** One entry in a right-click menu. */
type MenuItem = { label: string; danger?: boolean; run: () => void };
type Menu = { x: number; y: number; items: MenuItem[] } | null;

/** What a rename sheet is currently editing. */
type Renaming =
  | { kind: "device"; current: string }
  | { kind: "contact"; account: string; current: string }
  | null;

export default function App() {
  const [me, setMe] = useState<Me | null>(null);
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [thread, setThread] = useState<Message[]>([]);
  const [peers, setPeers] = useState(0);
  const [sheet, setSheet] = useState<Sheet>("none");
  const [fault, setFault] = useState<string | null>(null);
  const [menu, setMenu] = useState<Menu>(null);
  const [renaming, setRenaming] = useState<Renaming>(null);
  const [busy, setBusy] = useState(false);
  /** The image being looked at full size, as a data URL. */
  const [viewing, setViewing] = useState<string | null>(null);

  const current = contacts.find((c) => c.account_id === active) ?? null;

  const refreshContacts = useCallback(async () => {
    setContacts(await api.listContacts());
  }, []);

  const refreshThread = useCallback(async (id: string | null) => {
    setThread(id ? await api.conversation(id) : []);
  }, []);

  /// Everything on screen, re-read from the Rust side. What the reload button
  /// does, and the honest answer to "is this stale?" — the app is event-driven,
  /// but an event can be missed and a person cannot tell the difference.
  const reload = useCallback(async () => {
    setBusy(true);
    setFault(null);
    try {
      setMe(await api.me());
      await refreshContacts();
      setPeers((await api.network()).connected);
      await refreshThread(active);
    } catch (e) {
      setFault(String(e));
    } finally {
      setBusy(false);
    }
  }, [active, refreshContacts, refreshThread]);

  useEffect(() => {
    (async () => {
      try {
        setMe(await api.me());
        await refreshContacts();
        setPeers((await api.network()).connected);
      } catch (e) {
        setFault(String(e));
      }
    })();
  }, [refreshContacts]);

  useEffect(() => {
    void refreshThread(active);
  }, [active, refreshThread]);

  // A message can land in any conversation, so refresh the list either way and
  // the thread only when it is the one on screen.
  useEffect(() => {
    const unlisten = onMessage((conversation) => {
      void refreshContacts();
      if (conversation === active) void refreshThread(active);
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, [active, refreshContacts, refreshThread]);

  useEffect(() => {
    const unlisten = onNetwork(setPeers);
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);

  const send = async (text: string) => {
    if (!active) return;
    setFault(null);
    try {
      await api.sendMessage(active, text);
      await refreshThread(active);
    } catch (e) {
      setFault(String(e));
    }
  };

  const clearChat = async (account: string) => {
    setFault(null);
    try {
      await api.clearChat(account);
      await refreshContacts();
      if (account === active) await refreshThread(active);
    } catch (e) {
      setFault(String(e));
    }
  };

  const rename = async (name: string) => {
    if (!renaming) return;
    setFault(null);
    try {
      if (renaming.kind === "device") {
        await api.renameDevice(name);
        setMe(await api.me());
      } else {
        await api.renameContact(renaming.account, name);
        await refreshContacts();
      }
      setRenaming(null);
    } catch (e) {
      setFault(String(e));
    }
  };

  const sendFile = async (file: File) => {
    if (!active) return;
    setFault(null);
    // Checked here so a file that was never going to be sent is refused before
    // it is read. Rust refuses it again, and that is the check that counts.
    if (file.size > MAX_FILE_BYTES) {
      setFault(
        `${file.name} is ${formatSize(file.size)} — Vega sends at most ${formatSize(MAX_FILE_BYTES)}.`,
      );
      return;
    }
    if (file.size === 0) {
      setFault(`${file.name} is empty.`);
      return;
    }
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      await api.sendFile(active, file.name, toBase64(bytes));
      await refreshThread(active);
    } catch (e) {
      setFault(String(e));
    }
  };

  return (
    <div className="app" data-view={active ? "chat" : "list"}>
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-row">
            <h1>Vega</h1>
            <button
              className="icon reload"
              data-busy={busy}
              title="Reload contacts, messages and peer count"
              aria-label="Reload"
              onClick={() => void reload()}
            >
              ⟳
            </button>
          </div>

          {me && (
            <>
              <div className="identity" title="Your account id">
                {me.account_id}
              </div>
              {/* The single thing people get wrong: an invite is not a request
                  that the other end can accept. Both sides have to add the
                  other's id before either can send anything. */}
              <p className="hint">
                Both ends need to add each other&apos;s ids before you can
                message each other.
              </p>

              <div className="device">
                <span className="device-name">{me.device_label}</span>
                <button
                  className="icon"
                  title="Rename this device (only you see this)"
                  aria-label="Rename this device"
                  onClick={() =>
                    setRenaming({ kind: "device", current: me.device_label })
                  }
                >
                  ✎
                </button>
              </div>
            </>
          )}

          <div className="status">
            <span className={peers > 0 ? "dot live" : "dot"} />
            {peers > 0
              ? `${peers} peer${peers === 1 ? "" : "s"} reachable`
              : "looking for peers"}
          </div>
        </div>

        <div className="contacts">
          {contacts.map((c) => (
            <button
              key={c.account_id}
              className="contact"
              aria-current={c.account_id === active}
              onClick={() => setActive(c.account_id)}
              onContextMenu={(e) => {
                e.preventDefault();
                setMenu({
                  x: e.clientX,
                  y: e.clientY,
                  items: [
                    {
                      label: "Rename…",
                      run: () =>
                        setRenaming({
                          kind: "contact",
                          account: c.account_id,
                          current: c.display_name,
                        }),
                    },
                    {
                      label: "Copy account id",
                      run: () =>
                        void navigator.clipboard.writeText(c.account_id),
                    },
                    {
                      label: "Clear this chat",
                      danger: true,
                      run: () => void clearChat(c.account_id),
                    },
                  ],
                });
              }}
            >
              <span className="name">{c.display_name || "unnamed"}</span>
              <span className="sub">{c.short_id}</span>
            </button>
          ))}
          {contacts.length === 0 && (
            <div className="empty">
              <h2>No contacts yet</h2>
              <p>
                Share your invite with someone, or paste theirs. There is no
                directory to search — that is the point.
              </p>
            </div>
          )}
        </div>

        <footer>
          <button onClick={() => setSheet("invite")}>My invite</button>
          <button onClick={() => setSheet("add")}>Add contact</button>
          <button
            className="icon"
            title="Stored history"
            aria-label="Stored history"
            onClick={() => setSheet("history")}
          >
            ☰
          </button>
        </footer>
      </aside>

      <main className="chat">
        {current ? (
          <>
            <header>
              <button className="back" onClick={() => setActive(null)}>
                ←
              </button>
              <h2>{current.display_name || current.short_id}</h2>
              <button
                className="icon"
                title="Rename (only you see this)"
                aria-label="Rename this contact"
                onClick={() =>
                  setRenaming({
                    kind: "contact",
                    account: current.account_id,
                    current: current.display_name,
                  })
                }
              >
                ✎
              </button>
              <div className="safety">
                safety number
                <br />
                {current.safety_number}
              </div>
            </header>
            <Thread
              messages={thread}
              onMenu={setMenu}
              onOpenImage={setViewing}
            />
            {fault && (
              <div className="error" style={{ padding: "0 1rem" }}>
                {fault}
              </div>
            )}
            <Composer onSend={send} onSendFile={sendFile} />
          </>
        ) : (
          <div className="empty">
            <h2>Pick a conversation</h2>
            <p>
              Messages are encrypted to each of a person&apos;s devices and
              travel over whatever path works — the local network first.
            </p>
          </div>
        )}
      </main>

      {sheet === "invite" && me && (
        <InviteSheet me={me} onClose={() => setSheet("none")} />
      )}
      {sheet === "add" && (
        <AddSheet
          onClose={() => setSheet("none")}
          onAdded={async (id) => {
            await refreshContacts();
            setActive(id);
            setSheet("none");
          }}
        />
      )}
      {sheet === "history" && (
        <HistorySheet
          onClose={() => setSheet("none")}
          onCleared={async () => {
            await refreshContacts();
            await refreshThread(active);
          }}
        />
      )}

      {renaming && (
        <RenameSheet
          what={renaming.kind === "device" ? "this device" : "this contact"}
          current={renaming.current}
          onClose={() => setRenaming(null)}
          onSave={rename}
        />
      )}

      {viewing && (
        <ImageViewer src={viewing} onClose={() => setViewing(null)} />
      )}
      {menu && <ContextMenu menu={menu} onClose={() => setMenu(null)} />}
    </div>
  );
}

/**
 * A right-click menu.
 *
 * Positioned where the click was, then pulled back inside the window if that
 * would put it off the edge — a menu opened near the bottom right is the common
 * case, not the exotic one.
 */
function ContextMenu({
  menu,
  onClose,
}: {
  menu: NonNullable<Menu>;
  onClose: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [at, setAt] = useState({ x: menu.x, y: menu.y });

  useEffect(() => {
    const rect = box.current?.getBoundingClientRect();
    if (!rect) return;
    setAt({
      x: Math.min(menu.x, window.innerWidth - rect.width - 8),
      y: Math.min(menu.y, window.innerHeight - rect.height - 8),
    });
  }, [menu.x, menu.y]);

  // Anything that is not a choice closes it: another click, Escape, a scroll.
  useEffect(() => {
    const close = () => onClose();
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("keydown", key);
    };
  }, [onClose]);

  return (
    <div
      ref={box}
      className="menu"
      style={{ left: at.x, top: at.y }}
      role="menu"
      // The window-wide listener above would otherwise close the menu before
      // the click reached the item that was aimed at.
      onClick={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {menu.items.map((item) => (
        <button
          key={item.label}
          role="menuitem"
          className={item.danger ? "danger" : undefined}
          onClick={() => {
            item.run();
            onClose();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}

/** Bytes as something a person reads, not as a number of bytes. */
function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB"];
  let size = bytes / 1024;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size < 10 ? size.toFixed(1) : Math.round(size)} ${units[unit]}`;
}

function Thread({
  messages,
  onMenu,
  onOpenImage,
}: {
  messages: Message[];
  onMenu: (menu: Menu) => void;
  onOpenImage: (src: string) => void;
}) {
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [messages]);

  const menuFor = (m: Message): MenuItem[] => {
    const items: MenuItem[] = [];
    if (m.file) {
      const file = m.file;
      if (file.image && file.ready) {
        items.push({
          label: "Open image",
          run: () => {
            void api.readImage(file.transfer).then(onOpenImage).catch(() => {});
          },
        });
      }
      if (file.ready) {
        items.push({
          label: "Save a copy",
          run: () => {
            void api.exportFile(file.transfer, file.name).catch(() => {});
          },
        });
      }
      items.push({
        label: "Copy file name",
        run: () => void navigator.clipboard.writeText(file.name),
      });
    } else {
      items.push({
        label: "Copy text",
        run: () => void navigator.clipboard.writeText(m.text),
      });
    }
    items.push({
      label: "Copy time sent",
      run: () =>
        void navigator.clipboard.writeText(new Date(m.at * 1000).toISOString()),
    });
    return items;
  };

  return (
    <div className="thread">
      {messages.map((m) => (
        <div
          key={m.id}
          className={m.outgoing ? "bubble mine" : "bubble"}
          onContextMenu={(e) => {
            e.preventDefault();
            onMenu({ x: e.clientX, y: e.clientY, items: menuFor(m) });
          }}
        >
          {m.file ? <FileBubble file={m.file} onOpen={onOpenImage} /> : m.text}
          <time>{new Date(m.at * 1000).toLocaleTimeString()}</time>
        </div>
      ))}
      <div ref={end} />
    </div>
  );
}

/**
 * Load a picture past this and the preview waits to be asked.
 *
 * Every image in view is held in memory as a base64 string, so a thread of
 * ten-megabyte photos would otherwise load eighty megabytes of text into the
 * page to show pictures nobody has looked at yet.
 */
const PREVIEW_LIMIT = 4 * 1024 * 1024;

function FileBubble({
  file,
  onOpen,
}: {
  file: NonNullable<Message["file"]>;
  onOpen: (src: string) => void;
}) {
  const [saved, setSaved] = useState<string | null>(null);
  const [src, setSrc] = useState<string | null>(null);
  const [fault, setFault] = useState(false);
  const done = file.ready;
  const isImage = done && file.image !== null;

  const load = useCallback(async () => {
    try {
      const url = await api.readImage(file.transfer);
      setSrc(url);
      return url;
    } catch {
      // A file that vanished, or one whose bytes stopped being an image. The
      // bubble falls back to being a plain file rather than showing nothing.
      setFault(true);
      return null;
    }
  }, [file.transfer]);

  useEffect(() => {
    if (isImage && !src && !fault && file.size <= PREVIEW_LIMIT) void load();
  }, [isImage, src, fault, file.size, load]);

  return (
    <div className="file">
      {isImage && !fault && (
        <div className="file-image">
          {src ? (
            <img
              src={src}
              alt={file.name}
              onClick={() => onOpen(src)}
              title="Open"
            />
          ) : (
            <button
              onClick={async () => {
                const url = await load();
                if (url) onOpen(url);
              }}
            >
              Show image ({formatSize(file.size)})
            </button>
          )}
        </div>
      )}

      <div className="file-name">{file.name}</div>
      <div className="file-meta">
        {done
          ? formatSize(file.size)
          : `${formatSize(file.size)} · piece ${file.have} of ${file.chunks}`}
      </div>
      {done ? (
        // What Vega stores is encrypted, so there is no path worth handing over
        // — saving a copy is what makes it a file anything else can open, and
        // that copy has only whatever protection the disk gives it.
        <button
          className="file-path"
          title={saved ?? "Write a decrypted copy to your downloads folder"}
          onClick={async () => {
            try {
              setSaved(await api.exportFile(file.transfer, file.name));
            } catch (e) {
              setFault(true);
              setSaved(String(e));
            }
          }}
        >
          {saved ? `Saved to ${saved}` : "Save a copy"}
        </button>
      ) : (
        <progress value={file.have} max={file.chunks} />
      )}
    </div>
  );
}

/**
 * A received image at full size, inside the app.
 *
 * Nothing is handed to the operating system: the picture is already in the page
 * as a `data:` URL, and this only makes it big.
 */
function ImageViewer({ src, onClose }: { src: string; onClose: () => void }) {
  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose]);

  return (
    <div className="viewer" onClick={onClose}>
      <img src={src} alt="" onClick={(e) => e.stopPropagation()} />
      <button
        className="icon viewer-close"
        aria-label="Close"
        onClick={onClose}
      >
        ✕
      </button>
    </div>
  );
}

function Composer({
  onSend,
  onSendFile,
}: {
  onSend: (text: string) => void;
  onSendFile: (file: File) => void;
}) {
  const [text, setText] = useState("");
  const picker = useRef<HTMLInputElement>(null);

  const submit = () => {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSend(trimmed);
    setText("");
  };

  return (
    <div className="composer">
      {/* The picker lives in the page rather than in a native dialog, which is
          what keeps the app's capability list empty: no filesystem plugin, no
          permission to read a path Vega was not handed. */}
      <input
        ref={picker}
        type="file"
        hidden
        onChange={(e) => {
          const file = e.target.files?.[0];
          if (file) onSendFile(file);
          // Cleared so picking the same file twice in a row still fires.
          e.target.value = "";
        }}
      />
      <button
        className="attach"
        title="Send a file"
        onClick={() => picker.current?.click()}
      >
        +
      </button>
      <textarea
        value={text}
        placeholder="Write a message"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          // Enter sends; Shift+Enter is a newline, as everywhere else.
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            submit();
          }
        }}
      />
      <button className="primary" onClick={submit} disabled={!text.trim()}>
        Send
      </button>
    </div>
  );
}

/**
 * Rename a contact or this device.
 *
 * Both are local: the account id is what identifies a contact on the wire, and
 * the device label inside the sigchain is signed and cannot be edited. Saying so
 * on the sheet matters — a rename that looked like it reached the other person
 * would be a lie about what they see.
 */
function RenameSheet({
  what,
  current,
  onClose,
  onSave,
}: {
  what: string;
  current: string;
  onClose: () => void;
  onSave: (name: string) => void;
}) {
  const [name, setName] = useState(current);

  return (
    <div className="sheet" onClick={onClose}>
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <h2>Rename {what}</h2>
        <p>
          This name is stored on this device and never sent. Nobody else sees it
          — not the person it refers to, and not the network.
        </p>
        <input
          autoFocus
          value={name}
          placeholder="A name you will recognise"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSave(name);
            if (e.key === "Escape") onClose();
          }}
        />
        <div className="row">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" onClick={() => onSave(name)}>
            Save
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Everything stored, and the means to delete it.
 *
 * Counts come from the message index rather than from reading the threads, so
 * opening this costs nothing on an account with years behind it.
 */
function HistorySheet({
  onClose,
  onCleared,
}: {
  onClose: () => void;
  onCleared: () => void;
}) {
  const [rows, setRows] = useState<History[] | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [fault, setFault] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setRows(await api.history());
    } catch (e) {
      setFault(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const clearOne = async (account: string) => {
    try {
      await api.clearChat(account);
      await load();
      onCleared();
    } catch (e) {
      setFault(String(e));
    }
  };

  const clearAll = async () => {
    try {
      await api.clearAllHistory();
      setConfirming(false);
      await load();
      onCleared();
    } catch (e) {
      setFault(String(e));
    }
  };

  const total = rows?.reduce((n, r) => n + r.messages, 0) ?? 0;

  return (
    <div className="sheet" onClick={onClose}>
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <h2>Stored history</h2>
        <p>
          Clearing a chat deletes its messages and any files that came with it,
          from this device. It does not reach the other person&apos;s copy, and
          it does not un-send anything already on its way.
        </p>

        <div className="history">
          {rows?.map((r) => (
            <div key={r.account_id} className="history-row">
              <span className="name">{r.display_name}</span>
              <span className="sub">
                {r.messages} message{r.messages === 1 ? "" : "s"}
              </span>
              <button onClick={() => void clearOne(r.account_id)}>Clear</button>
            </div>
          ))}
          {rows?.length === 0 && <p className="sub">Nothing stored yet.</p>}
          {rows === null && <p className="sub">Reading…</p>}
        </div>

        {fault && <div className="error">{fault}</div>}

        <div className="row">
          <button onClick={onClose}>Close</button>
          {confirming ? (
            // Two steps, because this is the one button in the application that
            // destroys something with no copy anywhere else.
            <button className="danger" onClick={() => void clearAll()}>
              Delete {total} message{total === 1 ? "" : "s"} — certain?
            </button>
          ) : (
            <button
              className="danger"
              disabled={total === 0}
              onClick={() => setConfirming(true)}
            >
              Clear everything
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

function InviteSheet({ me, onClose }: { me: Me; onClose: () => void }) {
  const [name, setName] = useState(me.device_label);
  const [invite, setInvite] = useState("");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    void api.myInvite(name).then(setInvite);
  }, [name]);

  return (
    <div className="sheet" onClick={onClose}>
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <h2>Your invite</h2>
        <p>
          This carries your account id, your contact key and your signed device
          list. Anyone who can swap it in transit can read what follows, so send
          it over a channel you already trust.
        </p>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Name to show"
        />
        <div className="code">{invite}</div>
        <div className="row">
          <button
            onClick={async () => {
              await navigator.clipboard.writeText(invite);
              setCopied(true);
            }}
          >
            {copied ? "Copied" : "Copy"}
          </button>
          <button className="primary" onClick={onClose}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}

function AddSheet({
  onClose,
  onAdded,
}: {
  onClose: () => void;
  onAdded: (id: string) => void;
}) {
  const [text, setText] = useState("");
  const [fault, setFault] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const add = async () => {
    setBusy(true);
    setFault(null);
    try {
      const contact = await api.addContact(text.trim());
      onAdded(contact.account_id);
    } catch (e) {
      setFault(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="sheet" onClick={onClose}>
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <h2>Add a contact</h2>
        <p>
          Paste an invite. It is verified before it is saved — the signed device
          list has to match the account it claims to be.
        </p>
        <textarea
          rows={5}
          value={text}
          placeholder="vega1:…"
          onChange={(e) => setText(e.target.value)}
        />
        {fault && <div className="error">{fault}</div>}
        <div className="row">
          <button onClick={onClose}>Cancel</button>
          <button
            className="primary"
            onClick={add}
            disabled={busy || !text.trim()}
          >
            {busy ? "Verifying" : "Add"}
          </button>
        </div>
      </div>
    </div>
  );
}
