import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  MAX_FILE_BYTES,
  onMessage,
  onNetwork,
  toBase64,
  type Contact,
  type Me,
  type Message,
} from "./api";

type Sheet = "none" | "invite" | "add";

export default function App() {
  const [me, setMe] = useState<Me | null>(null);
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [thread, setThread] = useState<Message[]>([]);
  const [peers, setPeers] = useState(0);
  const [sheet, setSheet] = useState<Sheet>("none");
  const [fault, setFault] = useState<string | null>(null);

  const current = contacts.find((c) => c.account_id === active) ?? null;

  const refreshContacts = useCallback(async () => {
    setContacts(await api.listContacts());
  }, []);

  const refreshThread = useCallback(async (id: string | null) => {
    setThread(id ? await api.conversation(id) : []);
  }, []);

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
          <h1>Vega</h1>
          {me && <div className="identity">{me.account_id}</div>}
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
              <div className="safety">
                safety number
                <br />
                {current.safety_number}
              </div>
            </header>
            <Thread messages={thread} />
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

function Thread({ messages }: { messages: Message[] }) {
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [messages]);

  return (
    <div className="thread">
      {messages.map((m) => (
        <div key={m.id} className={m.outgoing ? "bubble mine" : "bubble"}>
          {m.file ? <FileBubble file={m.file} /> : m.text}
          <time>{new Date(m.at * 1000).toLocaleTimeString()}</time>
        </div>
      ))}
      <div ref={end} />
    </div>
  );
}

function FileBubble({ file }: { file: NonNullable<Message["file"]> }) {
  const [copied, setCopied] = useState(false);
  const done = file.path !== null;

  return (
    <div className="file">
      <div className="file-name">{file.name}</div>
      <div className="file-meta">
        {done
          ? formatSize(file.size)
          : `${formatSize(file.size)} · piece ${file.have} of ${file.chunks}`}
      </div>
      {done ? (
        // No opener: granting the app a shell or filesystem plugin to launch
        // whatever a contact sent would be a strange thing for this program to
        // do. The path is here to be copied into whatever you trust to open it.
        <button
          className="file-path"
          title={file.path ?? ""}
          onClick={async () => {
            await navigator.clipboard.writeText(file.path ?? "");
            setCopied(true);
          }}
        >
          {copied ? "Path copied" : "Copy path"}
        </button>
      ) : (
        <progress value={file.have} max={file.chunks} />
      )}
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
