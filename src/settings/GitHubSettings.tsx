import { useMutation, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { OrgAuth } from "../bindings/OrgAuth";
import type { OrgConfig } from "../bindings/OrgConfig";
import type { OrgStatusView } from "../bindings/OrgStatusView";
import type { RepoScope } from "../bindings/RepoScope";
import { Button, Card, ErrorText, Pill, TimeAgo } from "../components/ui";
import { CopyValue } from "../components/CopyValue";
import { commands, isTauri } from "../lib/tauri";

export function GitHubSettings({ orgs, status }: { orgs: OrgConfig[]; status: OrgStatusView[] }) {
  const statusFor = (login: string) => status.find((o) => o.login.toLowerCase() === login.toLowerCase());
  return (
    <>
      <h2 className="text-[13px] font-semibold">GitHub organizations</h2>
      <p className="mt-1 text-[12px] text-ink-2">
        Read-only. Tokens are stored in your macOS Keychain and never sent to runner machines.
      </p>
      <div className="mt-3 space-y-2">
        {orgs.map((o) => (
          <OrgRow key={o.login} org={o} status={statusFor(o.login)} />
        ))}
      </div>
      <AddOrg />
    </>
  );
}

function scopeText(s: RepoScope): string {
  switch (s.kind) {
    case "recent":
      return `${s.maxRepos} most recently active repositories`;
    case "selected":
      return s.repos.length ? s.repos.join(", ") : "no repositories";
    case "none":
      return "runner status only (no job scan)";
  }
}

function OrgRow({ org, status }: { org: OrgConfig; status?: OrgStatusView }) {
  const [mode, setMode] = useState<"view" | "token" | "scope" | "remove">("view");
  const [token, setToken] = useState("");
  const [scope, setScope] = useState<RepoScope>(org.repoScope);
  const save = useMutation({
    mutationFn: (command: () => Promise<void>) => command(),
    onSuccess: () => {
      setMode("view");
      setToken("");
    },
  });
  const show = (next: typeof mode) => {
    save.reset();
    setToken("");
    setScope(org.repoScope);
    setMode(next);
  };

  return (
    <Card className="px-4 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <CopyValue value={org.login} label="Organization" className="font-medium" truncate />
        {status?.error ? <Pill tone="bad">Error</Pill> : status?.runnersAsOf ? <Pill tone="ok">Connected</Pill> : <Pill tone="muted">Pending</Pill>}
        <div className="ml-auto flex flex-wrap gap-1">
          {org.auth === "token" ? (
            <Button small variant="ghost" disabled={save.isPending} onClick={() => save.mutate(() => commands.useGhForOrg(org.login))}>
              Use gh login
            </Button>
          ) : null}
          <Button small variant="ghost" onClick={() => show("token")}>
            {org.auth === "token" ? "Token…" : "Use a token…"}
          </Button>
          <Button small variant="ghost" onClick={() => show("scope")}>
            Scope…
          </Button>
          <Button small variant="ghost" onClick={() => show("remove")}>
            Remove…
          </Button>
        </div>
      </div>
      <div className="mt-1 break-words text-[11.5px] text-ink-2">
        {org.auth === "ghCli" ? "Signed in with the GitHub CLI (gh)" : "Token in Keychain"} · Jobs: {scopeText(org.repoScope)}
        {status?.runnersAsOf && <> · runners checked <TimeAgo iso={status.runnersAsOf} /></>}
        {status?.rateLimit && ` · API ${status.rateLimit.remaining}/${status.rateLimit.limit} left`}
      </div>
      {status?.error && <div className="mt-1 break-words text-[11.5px] text-bad selectable">{status.error}</div>}
      {status?.jobsTruncated && (
        <div className="mt-1 text-[11.5px] text-warn">Some repositories couldn't be scanned for jobs (no access, or over the 50 repo cap).</div>
      )}

      {mode === "token" && (
        <form className="mt-3 flex gap-2" onSubmit={(e) => { e.preventDefault(); save.mutate(() => commands.updateOrgToken(org.login, token)); }}>
          <input className="input min-w-0 flex-1" type="password" placeholder="New token" value={token} onChange={(e) => setToken(e.target.value)} autoFocus />
          <Button type="submit" variant="primary" disabled={!token || save.isPending}>
            {save.isPending ? "Checking…" : "Save"}
          </Button>
          <Button variant="ghost" onClick={() => show("view")}>
            Cancel
          </Button>
        </form>
      )}
      {mode === "scope" && (
        <form className="mt-3" onSubmit={(e) => { e.preventDefault(); save.mutate(() => commands.setOrgScope(org.login, scope)); }}>
          <ScopePicker value={scope} onChange={setScope} />
          <div className="mt-2 flex gap-2">
            <Button type="submit" variant="primary" disabled={save.isPending}>
              Save
            </Button>
            <Button variant="ghost" onClick={() => show("view")}>
              Cancel
            </Button>
          </div>
        </form>
      )}
      {mode === "remove" && (
        <div className="mt-3 flex flex-wrap items-center gap-2 text-[12px]">
          <span className="text-ink-2">Remove {org.login} and delete its token from the Keychain?</span>
          <Button small variant="danger" disabled={save.isPending} onClick={() => save.mutate(() => commands.removeOrg(org.login))}>
            Remove
          </Button>
          <Button small variant="ghost" onClick={() => show("view")}>
            Cancel
          </Button>
        </div>
      )}
      {save.isError && <ErrorText error={save.error} className="mt-2" />}
    </Card>
  );
}

function ScopePicker({ value, onChange }: { value: RepoScope; onChange: (s: RepoScope) => void }) {
  const [max, setMax] = useState(value.kind === "recent" ? value.maxRepos : 20);
  const [repos, setRepos] = useState(value.kind === "selected" ? value.repos.join("\n") : "");
  const parseRepos = (t: string) =>
    t
      .split(/[\s,]+/)
      .map((r) => r.trim().split("/").pop()!)
      .filter(Boolean);
  return (
    <div className="space-y-1.5 text-[12.5px]">
      <label className="flex items-center gap-2">
        <input type="radio" checked={value.kind === "recent"} onChange={() => onChange({ kind: "recent", maxRepos: max })} />
        <span>The</span>
        <input
          className="input w-[56px]"
          type="number"
          min={1}
          max={50}
          value={max}
          onChange={(e) => {
            const n = Math.max(1, Math.min(50, Number(e.target.value) || 1));
            setMax(n);
            onChange({ kind: "recent", maxRepos: n });
          }}
        />
        <span>most recently active repositories</span>
      </label>
      <label className="flex items-start gap-2">
        <input className="mt-1" type="radio" checked={value.kind === "selected"} onChange={() => onChange({ kind: "selected", repos: parseRepos(repos) })} />
        <div className="flex-1">
          <span>These repositories</span>
          {value.kind === "selected" && (
            <textarea
              className="input mt-1 h-[64px] py-1"
              placeholder="web, api, ios-app"
              value={repos}
              onChange={(e) => {
                setRepos(e.target.value);
                onChange({ kind: "selected", repos: parseRepos(e.target.value) });
              }}
            />
          )}
        </div>
      </label>
      <label className="flex items-center gap-2">
        <input type="radio" checked={value.kind === "none"} onChange={() => onChange({ kind: "none" })} />
        <span>Runner status only (no jobs or queue)</span>
      </label>
      <p className="text-[11.5px] text-ink-3">
        GitHub has no organization-wide job list, so each repository in scope costs a few API calls per check. Unchanged
        responses don't count against the rate limit.
      </p>
    </div>
  );
}

function GhStatusLine() {
  const { data: gh, refetch, isFetching } = useQuery({ queryKey: ["gh-status"], queryFn: commands.getGhStatus });
  if (!gh) return <div className="text-[12px] text-ink-3">Checking the GitHub CLI…</div>;
  return (
    <div className="rounded-[8px] border border-line bg-sunken px-3 py-2 text-[12px]">
      <div className="flex items-center gap-2">
        {gh.login && !gh.missingRunnersScope ? (
          <Pill tone="ok">gh: {gh.login}</Pill>
        ) : gh.login ? (
          <Pill tone="warn">gh: {gh.login}</Pill>
        ) : (
          <Pill tone="bad">{gh.installed ? "gh not logged in" : "gh not installed"}</Pill>
        )}
        <span className="min-w-0 truncate text-ink-3">{gh.scopes.length ? `scopes: ${gh.scopes.join(", ")}` : gh.path}</span>
        <button className="ml-auto shrink-0 text-accent hover:underline" onClick={() => refetch()} disabled={isFetching}>
          {isFetching ? "Checking…" : "Check again"}
        </button>
      </div>
      {gh.problem && <div className="mt-1 text-ink-2">{gh.problem}</div>}
      {gh.fix && (
        <div className="mt-1 break-words">
          Run in Terminal: <CopyValue value={gh.fix} label="Command"><code className="rounded bg-card px-1">{gh.fix}</code></CopyValue>
        </div>
      )}
    </div>
  );
}

function AddOrg() {
  const [open, setOpen] = useState(false);
  const [login, setLogin] = useState("");
  const [auth, setAuth] = useState<OrgAuth>("ghCli");
  const [token, setToken] = useState("");
  const [scope, setScope] = useState<RepoScope>({ kind: "recent", maxRepos: 20 });
  const add = useMutation({
    mutationFn: () => commands.addOrg(login, auth, auth === "token" ? token : null, scope),
    onSuccess: () => {
      setOpen(false);
      setLogin("");
      setToken("");
    },
  });

  if (!open) {
    return (
      <div className="mt-2">
        <Button onClick={() => setOpen(true)} disabled={!isTauri}>
          Add organization…
        </Button>
      </div>
    );
  }
  return (
    <Card className="mt-2 px-4 py-3.5">
      <form
        className="space-y-3"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <Field label="Organization">
          <input className="input" placeholder="acme" value={login} onChange={(e) => setLogin(e.target.value)} autoFocus />
        </Field>
        <Field label="Sign in">
          <div className="space-y-2 text-[12.5px]">
            <label className="flex items-center gap-2">
              <input type="radio" checked={auth === "ghCli"} onChange={() => setAuth("ghCli")} />
              Use my GitHub CLI login (gh)
            </label>
            {auth === "ghCli" && <GhStatusLine />}
            <label className="flex items-center gap-2">
              <input type="radio" checked={auth === "token"} onChange={() => setAuth("token")} />
              Paste a token instead
            </label>
            {auth === "token" && (
              <>
                <input className="input" type="password" placeholder="github_pat_…" value={token} onChange={(e) => setToken(e.target.value)} />
                <p className="text-[11.5px] text-ink-2">
                  Fine-grained token with this organization as resource owner: organization permission{" "}
                  <b>Self-hosted runners: Read</b> and repository permission <b>Actions: Read</b>. Stored in your Keychain.
                </p>
              </>
            )}
          </div>
        </Field>
        <Field label="Jobs and queue">
          <ScopePicker value={scope} onChange={setScope} />
        </Field>
        <div className="flex items-center gap-2">
          <Button type="submit" variant="primary" disabled={!login || (auth === "token" && !token) || add.isPending}>
            {add.isPending ? "Checking access…" : "Add"}
          </Button>
          <Button variant="ghost" onClick={() => setOpen(false)}>
            Cancel
          </Button>
        </div>
        {add.isError && <ErrorText error={add.error} />}
      </form>
    </Card>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="mb-1 text-[12px] font-medium">{label}</div>
      {children}
    </div>
  );
}
