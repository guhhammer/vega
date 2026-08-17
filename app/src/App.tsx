import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  MAX_FILE_BYTES,
  onMessage,
  onNetwork,
  toBase64,
  type Contact,
  type History,
  type InvitePreview,
  type Me,
  type Message,
} from "./api";

type Sheet = "none" | "invite" | "add" | "history";

/** One entry in a right-click menu. */
type MenuItem = { label: string; danger?: boolean; run: () => void };
type Menu = { x: number; y: number; items: MenuItem[] } | null;

/** The text selected anywhere on the page, if there is any. */
function selection(): string {
  return (window.getSelection()?.toString() ?? "").trim();
}

/**
 * Cut, copy, paste, select all — the menu a text field is expected to have.
 *
 * The web view's own menu is suppressed everywhere (see the root handler in
 * `App`), and a text field is the one place it was genuinely useful. Taking it
 * away without putting this back would make pasting an invite harder than it
 * was before, which is the opposite of the point.
 *
 * `insertText` rather than assigning to `value`: every field here is
 * React-controlled, and a direct assignment changes the box without telling the
 * component, so the next render quietly puts the old text back. Going through
 * the editing command raises the same `input` event typing would.
 */
function fieldItems(
  field: HTMLInputElement | HTMLTextAreaElement,
  fail: (message: string) => void,
): MenuItem[] {
  const start = field.selectionStart ?? 0;
  const end = field.selectionEnd ?? 0;
  const chosen = field.value.slice(start, end);
  const items: MenuItem[] = [];

  // Clicking a menu item moves focus off the field and takes the selection
  // with it, so both are put back before anything is edited.
  const restore = () => {
    field.focus();
    field.setSelectionRange(start, end);
  };

  if (chosen) {
    items.push({
      label: "Cut",
      run: () => {
        void navigator.clipboard.writeText(chosen);
        restore();
        document.execCommand("delete");
      },
    });
    items.push({
      label: "Copy",
      run: () => void navigator.clipboard.writeText(chosen),
    });
  }

  items.push({
    label: "Paste",
    run: () => {
      void navigator.clipboard
        .readText()
        .then((text) => {
          if (!text) return;
          restore();
          document.execCommand("insertText", false, text);
        })
        .catch(() =>
          // Some web views refuse to hand a page the clipboard at all. Saying
          // so is the whole of what can be done here — Ctrl+V still works,
          // because that path never goes through the page.
          fail("This web view will not let Vega read the clipboard. Ctrl+V."),
        );
    },
  });

  if (field.value) {
    items.push({
      label: "Select all",
      run: () => {
        field.focus();
        field.select();
      },
    });
  }

  return items;
}

/** What a rename sheet is currently editing. */
type Renaming =
  | { kind: "device"; current: string }
  | { kind: "contact"; account: string; current: string }
  | null;

/* ------------------------------------------------------------------ people */

/**
 * A stable hue for an account id.
 *
 * Taken from the id and never from the name. A name is a local label anybody
 * can change, so colouring by name would let two people be made to look alike
 * by renaming one of them — which is the same substitution the safety words
 * exist to catch, and it should not be possible to walk into it by accident.
 * Lightness and saturation are fixed in CSS, so whichever hue falls out is
 * legible in both themes.
 */
function hueOf(accountId: string): number {
  let hue = 0;
  for (const ch of accountId) hue = (hue * 31 + ch.charCodeAt(0)) % 360;
  return hue;
}

/** The one or two characters that go on the disc. */
function glyphOf(name: string, accountId: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  if (words.length > 1) return words[0][0] + words[1][0];
  if (words.length === 1) return words[0].slice(0, 2);
  // No name yet — the start of the id, which is what the row shows underneath.
  return accountId.replace(/-/g, "").slice(0, 2);
}

/**
 * Somebody as a coloured disc.
 *
 * A list of contacts is a list of near-identical text rows, and near-identical
 * is exactly the wrong thing for a list of people you are about to send secrets
 * to. Colour and two letters make each row recognisable before it is read.
 */
function Avatar({
  id,
  name,
  size,
}: {
  id: string;
  name?: string;
  size?: "sm" | "lg";
}) {
  return (
    <span
      className={size ? `avatar ${size}` : "avatar"}
      style={{ "--hue": hueOf(id) } as React.CSSProperties}
      aria-hidden="true"
    >
      {glyphOf(name ?? "", id)}
    </span>
  );
}

/* ------------------------------------------------------------------- theme */

type Theme = "light" | "dark" | "system";

const THEME_KEY = "vega:theme";

/**
 * Light, dark, or whatever the machine says.
 *
 * "System" is the default and works by removing the attribute entirely, which
 * is what leaves the CSS media query in charge. A theme chosen outright is
 * remembered: somebody who has told a messenger to be dark has told it once.
 */
function useTheme(): [Theme, (next: Theme) => void] {
  const [theme, setTheme] = useState<Theme>(
    () => (localStorage.getItem(THEME_KEY) as Theme | null) ?? "system",
  );

  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") {
      root.removeAttribute("data-theme");
      localStorage.removeItem(THEME_KEY);
    } else {
      root.setAttribute("data-theme", theme);
      localStorage.setItem(THEME_KEY, theme);
    }
  }, [theme]);

  return [theme, setTheme];
}

function ThemeToggle({
  theme,
  onPick,
}: {
  theme: Theme;
  onPick: (next: Theme) => void;
}) {
  const options: [Theme, string, string][] = [
    ["light", "☀", "Light"],
    ["dark", "☾", "Dark"],
    ["system", "◐", "Match the system"],
  ];

  return (
    <div className="theme-toggle" role="radiogroup" aria-label="Theme">
      {options.map(([value, glyph, label]) => (
        <button
          key={value}
          role="radio"
          aria-checked={theme === value}
          aria-label={label}
          title={label}
          onClick={() => onPick(value)}
        >
          {glyph}
        </button>
      ))}
    </div>
  );
}

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
  const [theme, setTheme] = useTheme();
  /** The contact whose safety words are being compared right now. */
  const [verifying, setVerifying] = useState<Contact | null>(null);
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

  /**
   * Record the answer to a safety-word comparison.
   *
   * The comparison happened on a call or across a table. All this does is
   * remember which way it went, so the conversation stops asking.
   */
  const answerVerification = async (account: string, matched: boolean) => {
    setFault(null);
    try {
      await api.setVerified(account, matched);
      await refreshContacts();
      setVerifying(null);
      if (!matched) {
        setFault(
          "Safety words that do not match mean somebody is in the middle. Delete this contact and exchange invites again over a different channel.",
        );
      }
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

  /**
   * Right-click, everywhere it was not already handled.
   *
   * The web view's own menu is the wrong menu: it offers to reload the document
   * and open a developer console, neither of which means anything inside an
   * application, and it offers nothing that does. So it is suppressed across the
   * whole window and replaced. Contacts and messages stop the event before it
   * gets here; a text field gets the editing menu it just lost; and everywhere
   * else — the sidebar, the header, the empty middle — used to be dead space and
   * now offers what you would otherwise cross the window to the footer for.
   */
  const onContextMenu = (e: React.MouseEvent) => {
    // A development build keeps one way through to the inspector.
    if (import.meta.env.DEV && e.shiftKey) return;
    e.preventDefault();

    const field = (e.target as HTMLElement).closest<
      HTMLInputElement | HTMLTextAreaElement
    >("input, textarea");

    let items: MenuItem[];
    if (field) {
      items = fieldItems(field, setFault);
    } else {
      const chosen = selection();
      items = [];
      if (chosen) {
        items.push({
          label: "Copy",
          run: () => void navigator.clipboard.writeText(chosen),
        });
      }
      items.push(
        { label: "My invite", run: () => setSheet("invite") },
        { label: "Add contact", run: () => setSheet("add") },
      );
      if (me) {
        items.push(
          {
            label: "Copy my account id",
            run: () => void navigator.clipboard.writeText(me.account_id),
          },
          {
            label: "Copy my words",
            run: () => void navigator.clipboard.writeText(me.identity_words),
          },
        );
      }
      items.push(
        { label: "Stored history", run: () => setSheet("history") },
        { label: "Reload", run: () => void reload() },
      );
    }

    setMenu({ x: e.clientX, y: e.clientY, items });
  };

  return (
    <div
      className="app"
      data-view={active ? "chat" : "list"}
      data-peers={peers > 0 ? "live" : "none"}
      onContextMenu={onContextMenu}
    >
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-row">
            <h1>Vega</h1>
            <ThemeToggle theme={theme} onPick={setTheme} />
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

          {/* Who you are, in the shape a contact takes further down the
              sidebar — the same disc, the same words. Seeing your own identity
              rendered exactly like everyone else's is the shortest way to
              understand that there is no account and no server here, only keys
              that look the same from either end.

              The 52-character id is deliberately not on screen. It is the same
              fact as the words below it, and only one of the two was ever going
              to be read down a phone line; the id itself is one right-click
              away for the times a machine needs it. */}
          {me && (
            <div className="me">
              <div className="me-row">
                <Avatar id={me.account_id} name={me.device_label} />
                <div className="me-names">
                  <span className="me-label">you</span>
                  <div className="me-device">
                    <span className="name" title={me.account_id}>
                      {me.device_label}
                    </span>
                  </div>
                </div>
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

              <button
                className="words"
                title="Your identity as ten words. Read them to whoever you send your invite to — if the words on their screen match, the invite reached them as you sent it. Click to copy."
                onClick={() =>
                  void navigator.clipboard.writeText(me.identity_words)
                }
              >
                <span className="words-label">your words</span>
                <span className="words-text">{me.identity_words}</span>
              </button>
            </div>
          )}

          <div
            className="status"
            title="Peers are anyone Vega is currently connected to — on this network or through a seed. They carry ciphertext they cannot read. You do not need one to add a contact, only to deliver a message."
          >
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
                // Otherwise the root handler runs next and replaces these with
                // the general menu, which is not what was aimed at.
                e.stopPropagation();
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
                      // Their own phrase, not the pairwise one: this is what
                      // they read out when they sent the invite, so it is what
                      // an invite that arrived wrong would fail to match.
                      label: "Copy their words",
                      run: () =>
                        void navigator.clipboard.writeText(c.identity_words),
                    },
                    {
                      label: c.verified
                        ? "Mark as unverified"
                        : "Compare safety words…",
                      run: () => setVerifying(c),
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
              <Avatar id={c.account_id} name={c.display_name} />
              <span className="contact-text">
                <span className="name">
                  {c.display_name || "unnamed"}
                  {c.verified && (
                    <span
                      className="check"
                      title="You compared safety words and they matched"
                    >
                      ✓
                    </span>
                  )}
                </span>
                <span className="sub">{c.short_id}</span>
              </span>
            </button>
          ))}

          {/* A first run lands here, and this is the whole of what somebody has
              to understand: there is no directory, and an invite is not a
              request the other end can accept. Both halves of the exchange are
              numbered and each is the button that performs it. */}
          {contacts.length === 0 && (
            <div className="empty">
              <span className="glyph" aria-hidden="true">
                ✦
              </span>
              <h2>Nobody here yet</h2>
              <p>
                There is no directory to search and no server to ask. You add
                each other by hand — both ends, or neither.
              </p>
              <div className="steps">
                <button className="step" onClick={() => setSheet("invite")}>
                  <span className="step-n">1</span>
                  <span className="step-text">
                    <span className="title">Send them your invite</span>
                    <span className="sub">and read your ten words aloud</span>
                  </span>
                </button>
                <button className="step" onClick={() => setSheet("add")}>
                  <span className="step-n">2</span>
                  <span className="step-text">
                    <span className="title">Paste theirs</span>
                    <span className="sub">
                      their words must match what they read you
                    </span>
                  </span>
                </button>
              </div>
            </div>
          )}
        </div>

        {/* Icon over label: three words in a row would wrap on a phone, and an
            icon on its own is a guess. The first action is the accented one
            until there is somebody to talk to, so a new window points at what
            to do next rather than sitting there evenly weighted. */}
        <footer>
          <button
            className={contacts.length === 0 ? "primary" : undefined}
            onClick={() => setSheet("invite")}
          >
            <span className="glyph" aria-hidden="true">
              ✦
            </span>
            My invite
          </button>
          <button onClick={() => setSheet("add")}>
            <span className="glyph" aria-hidden="true">
              ＋
            </span>
            Add contact
          </button>
          <button
            title="Everything stored on this device, and the means to delete it"
            onClick={() => setSheet("history")}
          >
            <span className="glyph" aria-hidden="true">
              ☰
            </span>
            Stored
          </button>
        </footer>
      </aside>

      <main className="chat">
        {current ? (
          <>
            <header>
              <button
                className="icon back"
                aria-label="Back to conversations"
                onClick={() => setActive(null)}
              >
                ←
              </button>
              <Avatar id={current.account_id} name={current.display_name} />
              <div className="chat-who">
                <h2>{current.display_name || current.short_id}</h2>
                <span className="chat-sub">{current.short_id}</span>
              </div>

              {/* The safety words used to sit here as a label, which meant the
                  one check in the application was something to look at rather
                  than something to do — and there was nowhere to record having
                  done it. This is the same fingerprint as a piece of state: it
                  asks while the answer is unknown, and says so once it is. */}
              <button
                className="verify"
                data-verified={current.verified}
                title={
                  current.verified
                    ? "You compared safety words and they matched. Click to go back over it."
                    : "Compare safety words with them on a call or in person. It is the only thing that rules out somebody having swapped your invites."
                }
                onClick={() => setVerifying(current)}
              >
                {current.verified ? "✓ verified" : "verify"}
              </button>

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
            </header>
            <Thread
              messages={thread}
              onMenu={setMenu}
              onOpenImage={setViewing}
            />
            <Composer onSend={send} onSendFile={sendFile} />
          </>
        ) : (
          <div className="empty">
            <span className="glyph" aria-hidden="true">
              ✧
            </span>
            <h2>Pick a conversation</h2>
            <p>
              Messages are encrypted to each of a person&apos;s devices and
              travel over whatever path works — the local network first, a relay
              only if it has to, and nothing along the way can read them.
            </p>
          </div>
        )}

        {/* Outside the branch above, because things that fail — a reload, a
            clipboard the web view will not hand over — fail just as often with
            no conversation open, and an error nobody is shown is an error
            nobody can act on. */}
        {fault && (
          <div className="error fault" onClick={() => setFault(null)}>
            {fault}
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

      {verifying && (
        <VerifySheet
          contact={verifying}
          onClose={() => setVerifying(null)}
          onAnswer={(matched) =>
            void answerVerification(verifying.account_id, matched)
          }
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
  //
  // Deliberately not `contextmenu`. That listener runs after the React handler
  // that opened the second menu, so right-clicking one contact while another's
  // menu was open closed both and left nothing — the menu appeared to swallow
  // the click. Every right-click inside the window now opens some menu, so
  // there is nothing left for that case to close anyway.
  useEffect(() => {
    const close = () => onClose();
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("click", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("click", close);
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
      // A right-click on the menu itself is neither a choice nor a request for
      // a different menu, so it stops here.
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
      }}
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

/* -------------------------------------------------------------- the thread */

/** Messages from the same side, closer together than this, read as one run. */
const RUN_GAP = 5 * 60;

function sameDay(a: number, b: number): boolean {
  return (
    new Date(a * 1000).toDateString() === new Date(b * 1000).toDateString()
  );
}

/**
 * Which day a run of messages belongs to.
 *
 * "Today" and "Yesterday" rather than a date, because those are the two a
 * person reads without doing arithmetic — and they are the two that come up.
 */
function dayLabel(at: number): string {
  const when = new Date(at * 1000);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);

  if (sameDay(at, today.getTime() / 1000)) return "Today";
  if (sameDay(at, yesterday.getTime() / 1000)) return "Yesterday";

  // A year in only when it is not this one: a date that says 2026 to somebody
  // reading a conversation from last week is noise.
  return when.toLocaleDateString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
    year: when.getFullYear() === today.getFullYear() ? undefined : "numeric",
  });
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

    // Part of a message is what somebody highlighting a phrase is after, and it
    // has to lead: "Copy text" below would quietly take the whole bubble.
    const chosen = selection();
    if (chosen) {
      items.push({
        label: "Copy selection",
        run: () => void navigator.clipboard.writeText(chosen),
      });
    }

    if (m.file) {
      const file = m.file;
      if (file.image && file.ready) {
        items.push({
          label: "Open image",
          run: () => {
            void api
              .readImage(file.transfer)
              .then(onOpenImage)
              .catch(() => {});
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

  if (messages.length === 0) {
    return (
      <div className="thread">
        <div className="empty">
          <span className="glyph" aria-hidden="true">
            ✧
          </span>
          <h2>Nothing here yet</h2>
          <p>
            Anything you send waits in an outbox until there is a way through,
            so it is worth writing even when nobody is reachable.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="thread">
      {messages.map((m, i) => {
        const previous = messages[i - 1];
        const next = messages[i + 1];

        const newDay = !previous || !sameDay(previous.at, m.at);
        // A run is one person talking without interruption. Grouping it means
        // the gaps in a conversation fall where the turns do, rather than
        // evenly between every line.
        const afterRun =
          !!previous &&
          !newDay &&
          previous.outgoing === m.outgoing &&
          m.at - previous.at < RUN_GAP;
        const beforeRun =
          !!next &&
          sameDay(m.at, next.at) &&
          next.outgoing === m.outgoing &&
          next.at - m.at < RUN_GAP;

        const run =
          afterRun && beforeRun
            ? "run-mid"
            : afterRun
              ? "run-last"
              : beforeRun
                ? "run-first"
                : "alone";

        return (
          <Fragment key={m.id}>
            {newDay && <div className="day">{dayLabel(m.at)}</div>}
            <div
              className={`bubble ${run}${m.outgoing ? " mine" : ""}`}
              onContextMenu={(e) => {
                e.preventDefault();
                // Otherwise the root handler runs next and replaces these with
                // the general menu, which is not what was aimed at.
                e.stopPropagation();
                onMenu({ x: e.clientX, y: e.clientY, items: menuFor(m) });
              }}
            >
              {m.file ? (
                <FileBubble file={m.file} onOpen={onOpenImage} />
              ) : (
                m.text
              )}
              <time>
                {new Date(m.at * 1000).toLocaleTimeString(undefined, {
                  hour: "2-digit",
                  minute: "2-digit",
                })}
              </time>
            </div>
          </Fragment>
        );
      })}
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
  const box = useRef<HTMLTextAreaElement>(null);

  // A message is a paragraph as often as it is a line, and a one-line box that
  // scrolls its own contents hides what you just wrote. Height follows the
  // text, up to the ceiling `max-height` sets, and then the box scrolls.
  // Reset to `auto` first, or shrinking back down after a deletion never
  // happens: scrollHeight cannot report less than the height already set.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [text]);

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
        className="icon attach"
        title="Send a file"
        aria-label="Send a file"
        onClick={() => picker.current?.click()}
      >
        ＋
      </button>
      <textarea
        ref={box}
        value={text}
        rows={1}
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
      <button
        className="primary send"
        onClick={submit}
        disabled={!text.trim()}
        aria-label="Send"
      >
        <span className="label">Send</span>
      </button>
    </div>
  );
}

/**
 * Compare safety words, and record the answer.
 *
 * Both renderings of one fingerprint are here, words first: the words are what
 * somebody will actually read down a phone line, and a check that gets skipped
 * protects nobody. The digits stay underneath for anyone who would rather
 * compare those.
 *
 * The two answers are deliberately not a confirm and a cancel. "They do not
 * match" is a real outcome with a real meaning, and a dialog that only offers a
 * way to agree teaches people to agree.
 */
function VerifySheet({
  contact,
  onClose,
  onAnswer,
}: {
  contact: Contact;
  onClose: () => void;
  onAnswer: (matched: boolean) => void;
}) {
  return (
    <div className="sheet" onClick={onClose}>
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <h2>Safety words</h2>
        <p>
          Get {contact.display_name || contact.short_id} on a call, or stand
          next to them, and read these to each other. Both screens show the same
          phrase when nobody has swapped your invites — and a different one when
          somebody has. Nothing else can tell you this.
        </p>

        <button
          className="words"
          title="Copy both renderings"
          onClick={() =>
            void navigator.clipboard.writeText(
              `${contact.safety_words}\n${contact.safety_number}`,
            )
          }
        >
          <span className="words-label">words</span>
          <span className="words-text">{contact.safety_words}</span>
          <span className="words-label">digits</span>
          <span className="words-text">{contact.safety_number}</span>
        </button>

        <p className="hint">
          This is a check on the pair of you, so it is not the same phrase as
          the ten words in your invite. Both are the same idea: a fingerprint
          short enough to say out loud.
        </p>

        <div className="row">
          <button className="grow" onClick={onClose}>
            Not now
          </button>
          <button className="danger" onClick={() => onAnswer(false)}>
            They do not match
          </button>
          <button className="primary" onClick={() => onAnswer(true)}>
            They match
          </button>
        </div>
      </div>
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
        {/* The check that belongs here rather than afterwards. Safety words
            need both accounts and so cannot exist until each has added the
            other — by which point a swapped invite is already the conversation.
            These need only this account, so they can be read out at the moment
            the invite is handed over, which is the moment it can be caught. */}
        <button
          className="words"
          title="Copy these words"
          onClick={() => void navigator.clipboard.writeText(me.identity_words)}
        >
          <span className="words-label">your words</span>
          <span className="words-text">{me.identity_words}</span>
        </button>
        <p>
          Read those ten words to whoever you are sending this to. They will see
          ten words computed from the invite that reached them: the same phrase
          means it arrived as you sent it, and a different one means it did not.
        </p>
        <p>
          Once you have each other, <strong>verify</strong> at the top of the
          conversation runs the same check for the pair of you and marks it
          done, so you are not left wondering later whether you ever did it.
        </p>
        <p className="hint">
          They have to add you as well. An invite is not a request the other end
          can accept — until both of you have added the other, neither can send
          anything.
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
  const [preview, setPreview] = useState<InvitePreview | null>(null);

  // Decoded as it is pasted, so the words are on screen while the person who
  // sent the invite is still on the call — reading them back afterwards, once
  // the contact is saved, is a check nobody performs. An invite that does not
  // decode simply shows nothing; `add` is what turns that into an error, since
  // half-typed text is not a mistake worth shouting about.
  useEffect(() => {
    const invite = text.trim();
    if (!invite) {
      setPreview(null);
      return;
    }
    let current = true;
    void api
      .previewInvite(invite)
      .then((p) => current && setPreview(p))
      .catch(() => current && setPreview(null));
    return () => {
      current = false;
    };
  }, [text]);

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

        {preview && (
          <div className="preview">
            <div className="preview-who">
              <Avatar
                id={preview.account_id}
                name={preview.display_name}
                size="sm"
              />
              <span className="name">{preview.display_name || "unnamed"}</span>
              <span className="sub">{preview.short_id}</span>
            </div>
            <button
              className="words"
              title="Copy these words"
              onClick={() =>
                void navigator.clipboard.writeText(preview.identity_words)
              }
            >
              <span className="words-label">their words</span>
              <span className="words-text">{preview.identity_words}</span>
            </button>
            {/* The one thing a manual exchange has to be able to catch, and the
                only moment it can be caught for free. */}
            <p className="hint">
              Read these back to whoever sent this. If they do not match the
              words on their screen, what arrived is not what they sent — do not
              add it.
            </p>
          </div>
        )}

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
