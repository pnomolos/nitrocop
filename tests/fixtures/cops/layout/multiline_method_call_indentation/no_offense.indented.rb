# nitrocop-config: EnforcedStyle: indented
# Matcher chains nested inside a non-parenthesized argument keep the outer
# visual indentation instead of indenting relative to the inner matcher.
expect { service.call }.to \
  change { visit.reload.name }
  .from("a")
  .to("b")

# Postfix conditional: `correct_indentation` gives no "special indentation",
# so the chain in the condition is indented by IndentationWidth alone.
def perform(follow)
  return if EmailMessage.where(user_id: follow.followable_id)
    .where("sent_at > ?", rand(15..35).hours.ago)
    .exists?
end

# `[]` / `[]=` are not parenthesized argument lists, so chains in their
# operands are still checked under every EnforcedStyle.
def link(result, record, record_id_to_repo_ids, repositories)
  result[record] = record_id_to_repo_ids
    .fetch(record.id, [])
    .map { |repo_ids| repositories.fetch(repo_ids) }
    .flatten
end
