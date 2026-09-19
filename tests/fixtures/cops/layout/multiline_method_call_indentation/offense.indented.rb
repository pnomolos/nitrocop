# nitrocop-config: EnforcedStyle: indented
# The plain matcher chain still uses normal 2-space indentation.
change { visit.reload.name }
.from("a")
^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
.to("b")
^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.

# Nested matcher chains do not inherit the outer expectation indentation.
expect { described_class.perform_now }.to \
  have_enqueued_job(Job)
    .with(1)
    ^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.
    .and have_enqueued_job(Job)
    ^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.
    .with(2)
    ^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.

# Prefix keyword: `correct_indentation` adds Layout/IndentationWidth's Width
# on top of this cop's IndentationWidth.
def check(follow)
  if EmailMessage.where(user_id: follow.followable_id)
    .where("sent_at > ?", rand(15..35).hours.ago)
    ^^^^^^ Layout/MultilineMethodCallIndentation: Use 4 (not 2) spaces for indenting a condition in an `if` statement spanning multiple lines.
    .exists?
    ^^^^^^^ Layout/MultilineMethodCallIndentation: Use 4 (not 2) spaces for indenting a condition in an `if` statement spanning multiple lines.
    true
  end
end
