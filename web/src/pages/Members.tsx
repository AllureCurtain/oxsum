import { useState } from "react";
import { ApiError } from "../api/client";
import {
  useAddMember,
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
  Select,
  TableShell,
  TextInput,
  Cr,
} from "../components/ui";

export function Members() {
  const session = useSession();
  const members = useMembers();
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

      {members.isLoading ? (
        <PageState>Loading…</PageState>
      ) : members.isError ? (
        <PageState>The members could not be read.</PageState>
      ) : (
        <TableShell head={["Email", "Role", "Joined", "Budget cap", ""]}>
          {(members.data?.members ?? []).map((member) => (
            <tr key={member.userId}>
              <td>{member.email}</td>
              <td>{member.role}</td>
              <td>{member.joinedAt.slice(0, 10)}</td>
              <td className="mono">
                {member.budgetLimitMinor != null ? <Cr minor={member.budgetLimitMinor} currency={display} /> : "—"}
              </td>
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
          {(members.data?.members.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>No members.</EmptyRows>
          ) : null}
        </TableShell>
      )}

      {canManage ? (
        <div className="grid grid-cols-1 gap-6 sm:grid-cols-2">
          <Card title="Add member">
            <form
              className="flex gap-2"
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
              Adds an existing account by email. Someone without an account joins through an
              invitation link instead.
            </p>
          </Card>
          <Card title="Invitation link">
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
          </Card>
        </div>
      ) : null}
    </div>
  );
}
