import { useState } from "react";
import { Link2, Users } from "lucide-react";
import { ApiError } from "../api/client";
import {
  useAddMember,
  useBudget,
  useCreateInvitation,
  useDisplay,
  useMembers,
  useRemoveMember,
  useSession,
  useTransferOwnership,
  useUpdateMember,
} from "../api/hooks";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  PageState,
  Progress,
  Select,
  TableShell,
  Td,
  TextInput,
  Cr,
} from "../components/ui";

export function Members() {
  const session = useSession();
  const members = useMembers();
  const budget = useBudget();
  const display = useDisplay();
  const add = useAddMember();
  const update = useUpdateMember();
  const remove = useRemoveMember();
  const transfer = useTransferOwnership();
  const invite = useCreateInvitation();

  const [email, setEmail] = useState("");
  const [inviteUrl, setInviteUrl] = useState<string | null>(null);

  const role = session.data?.role ?? "member";
  const canManage = role === "owner" || role === "admin";
  const isOwner = role === "owner";
  const ownId = session.data?.user.id;

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Members</h1>
        <span className="text-xs text-muted">Your role: {role}</span>
      </div>

      {inviteUrl ? (
        <Banner tone="success">
          Invitation link (valid once): <code className="mono">{inviteUrl}</code>
        </Banner>
      ) : null}
      {add.error instanceof ApiError ? <Banner tone="error">{add.error.message}</Banner> : null}

      {budget.data ? (
        <Card title="Budgets at a glance">
          <div className="flex flex-wrap gap-x-10 gap-y-4">
            {budget.data.organization.creditLimitMinor != null &&
            budget.data.organization.creditLimitMinor > 0 ? (
              <div className="min-w-60 flex-1">
                <div className="flex items-baseline justify-between text-sm">
                  <span className="text-xs font-medium uppercase tracking-wider text-muted">
                    Organization credit line
                  </span>
                  <span className="text-muted">
                    {budget.data.organization.consumedPercent ?? 0}% committed
                  </span>
                </div>
                <div className="mt-1 flex items-baseline gap-1.5 text-lg font-semibold tabular-nums text-ink">
                  <Cr minor={budget.data.organization.creditUsedMinor ?? 0} currency={display} />
                  <span className="text-sm font-normal text-muted">
                    / <Cr minor={budget.data.organization.creditLimitMinor} currency={display} />
                  </span>
                </div>
                <Progress
                  value={Math.min(100, budget.data.organization.consumedPercent ?? 0)}
                  className="mt-1.5 w-full"
                />
              </div>
            ) : (
              <div>
                <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
                  Organization balance
                </p>
                <p className="mt-1 text-lg font-semibold tabular-nums text-ink">
                  <Cr minor={budget.data.organization.availableMinor} currency={display} />
                </p>
                <p className="mt-0.5 text-xs text-muted">no credit line</p>
              </div>
            )}
            <div>
              <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
                Members
              </p>
              <p className="mt-1 text-lg font-semibold tabular-nums text-ink">
                {members.data?.members.length ?? "—"}
              </p>
              <p className="mt-0.5 text-xs text-muted">
                {(members.data?.members ?? []).filter((m) => m.role === "owner").length} owner
                {(members.data?.members ?? []).filter((m) => m.role === "admin").length
                  ? ` · ${(members.data?.members ?? []).filter((m) => m.role === "admin").length} admin`
                  : ""}
              </p>
            </div>
            <div>
              <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
                With budget cap
              </p>
              <p className="mt-1 text-lg font-semibold tabular-nums text-ink">
                {(members.data?.members ?? []).filter((m) => m.budgetLimitMinor != null).length}
              </p>
              <p className="mt-0.5 text-xs text-muted">
                {(members.data?.members ?? []).filter((m) => m.budgetLimitMinor == null).length} uncapped
              </p>
            </div>
          </div>
        </Card>
      ) : null}

      {members.isError ? (
        <PageState>The members could not be read.</PageState>
      ) : (
        <TableShell
          head={["Email", "Role", "Joined", { label: "Budget cap", align: "right" }, ""]}
          loading={members.isLoading ? 4 : 0}
        >
          {!members.isLoading &&
            (members.data?.members ?? []).map((member) => (
              <tr key={member.userId}>
                <td>{member.email}</td>
                <td>{member.role}</td>
                <td>{member.joinedAt.slice(0, 10)}</td>
                <Td mono align="right">
                  {member.budgetLimitMinor != null ? <Cr minor={member.budgetLimitMinor} currency={display} /> : "—"}
                </Td>
                <td className="text-right">
                  {canManage && member.userId !== ownId ? (
                    <span className="inline-flex items-center gap-2">
                      {member.role !== "owner" ? (
                        <Select
                          defaultValue={member.role}
                          onChange={(e) =>
                            update.mutate({ userId: member.userId, role: e.target.value })
                          }
                          aria-label={`role for ${member.email}`}
                        >
                          <option value="admin">admin</option>
                          <option value="member">member</option>
                        </Select>
                      ) : null}
                      <Button tone="danger" onClick={() => remove.mutate(member.userId)}>
                        Remove
                      </Button>
                      {isOwner ? (
                        <Button tone="ghost" onClick={() => transfer.mutate(member.userId)}>
                          Make owner
                        </Button>
                      ) : null}
                    </span>
                  ) : null}
                </td>
              </tr>
            ))}
          {!members.isLoading && (members.data?.members.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5} icon={<Users />}>
              No members yet — add one below.
            </EmptyRows>
          ) : null}
        </TableShell>
      )}

      {canManage ? (
        <Card title="Add member">
          <form
            className="flex max-w-md gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              add.mutate(email, { onSuccess: () => setEmail("") });
            }}
          >
            <Field label="Email">
              <TextInput
                type="email"
                required
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
            </Field>
            <div className="flex items-end">
              <Button type="submit" disabled={add.isPending}>
                Add
              </Button>
            </div>
          </form>
          <p className="mt-2 text-xs text-muted">
            Adds an existing account by email.
          </p>

          {/* The alternative path — someone without an account — collapses
              behind a details, so the common action stays primary. */}
          <details className="mt-4 border-t border-line pt-3">
            <summary className="flex cursor-pointer list-none items-center gap-2 text-sm font-medium text-muted hover:text-ink [&::-webkit-details-marker]:hidden">
              <Link2 className="size-4" /> Invite someone without an account
            </summary>
            <div className="mt-3">
              <Button
                tone="ghost"
                disabled={invite.isPending}
                onClick={() =>
                  invite.mutate(undefined, { onSuccess: (created) => setInviteUrl(created.url) })
                }
              >
                Create invitation link
              </Button>
              {invite.error instanceof ApiError ? (
                <p className="mt-2 text-xs text-danger">{invite.error.message}</p>
              ) : null}
            </div>
          </details>
        </Card>
      ) : null}
    </div>
  );
}
