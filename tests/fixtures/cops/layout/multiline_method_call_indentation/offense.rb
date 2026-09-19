foo
  .bar
    .baz
    ^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.

thing
  .first
  .second
      .third
      ^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 6) spaces for indenting an expression spanning multiple lines.

query
  .select('foo')
  .where(x: 1)
    .order(:name)
    ^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.

# Block chain continuation: .sort_by should align with .with_index dot
frequencies.map.with_index { |f, i| [f / total, hex[i]] }
           .sort_by { |r| -r[0] }
           ^^^ Layout/MultilineMethodCallIndentation: Align `.sort_by` with `.with_index` on line 16.

# Multiline receiver chain with single-line block: .sort_by should align with .with_index dot
submission.template_submitters
          .group_by.with_index { |s, index| s['order'] || index }
          .sort_by(&:first).pluck(1)
          ^^^ Layout/MultilineMethodCallIndentation: Align `.sort_by` with `.with_index` on line 21.

# Hash pair value: chain should align with chain root start column
foo(key: receiver.chained
                          .misaligned)
                          ^^^ Layout/MultilineMethodCallIndentation: Align `.misaligned` with `receiver.chained` on line 25.

bar = Foo
  .a
  ^^ Layout/MultilineMethodCallIndentation: Align `.a` with `Foo` on line 28.
      .b(c)

# Trailing dot: unaligned methods (aligned style)
User.a
  .b
  ^^ Layout/MultilineMethodCallIndentation: Align `.b` with `.a` on line 33.
 .c
 ^^ Layout/MultilineMethodCallIndentation: Align `.c` with `.a` on line 33.

# Trailing dot: misaligned in assignment
a = b.c.
 d
 ^ Layout/MultilineMethodCallIndentation: Align `d` with `b.c.` on line 38.

# Unaligned method in block body
a do
  b.c
    .d
    ^^ Layout/MultilineMethodCallIndentation: Align `.d` with `.c` on line 43.
end

# Hash pair value: misaligned multi-dot chain
method(key: value.foo.bar
                    .baz)
                    ^^^^ Layout/MultilineMethodCallIndentation: Align `.baz` with `value.foo.bar` on line 48.

# Aligned style fallback: implicit receiver chain with no indent
where("first_condition")
.where("second_condition")
^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.

# Block continuation: .map has block, aligned with receiver's continuation dot
# RuboCop accepts .map because find_continuation_node returns .select's dot
def foo
  MyClass.all
    .select("name")
    ^^ Layout/MultilineMethodCallIndentation: Align `.select` with `.all` on line 58.
    .map { |e| e.name }
end

# Block-pass receiver: RuboCop does not treat `&:strip` as a block for alignment
def ignored_organisations_string=(organisations_string)
  self.ignored_organisations = (organisations_string || "")
    .split(",")
    ^^^^^^ Layout/MultilineMethodCallIndentation: Align `.split` with `(organisations_string || "")` on line 65.
    .collect(&:strip)
    ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.collect` with `(organisations_string || "")` on line 65.
      .compact
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.compact` with `(organisations_string || "")` on line 65.
end

# []= receiver: square brackets are not parenthesized arg lists
def prepare_headers
  headers['Cookie'] = final_cookies_hash.
    map { |k, v| "#{Cookie.encode(k)}=#{Cookie.encode(v)}" }.join(';')
    ^^^ Layout/MultilineMethodCallIndentation: Align `map` with `final_cookies_hash.` on line 73.
end

# Trailing-dot setter call: RuboCop checks setter methods like ordinary chains
trigger = proc do
  described_class.new(url: url, inputs: { name: 'value' }).
      nonce_name = 'stuff'
      ^^^^^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.
end

# Repeated continuation dots should not inherit a bad column from the first one
def self.pull_request_filter
  where("contributions.user_id = aggregation_filters.user_id")
  .where("contributions.title ILIKE aggregation_filters.title_pattern")
  ^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
  .arel.exists.not
  ^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
end

# Long leading-dot chains: later continuations still use the base indentation
def organisation_roles(type)
  @organisation
  .organisation_roles
  ^^^^^^^^^^^^^^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
  .joins(:role)
  ^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
  .merge(roles_for_type(type))
  ^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
  .order(:ordering)
  ^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 0) spaces for indenting an expression spanning multiple lines.
end

# RSpec stub chain: later dots still use the chain indent when the first continuation is wrong
before do
  allow(SteamCondenser::Community::SteamId).to receive(:steam_id_to_community_id)
                                              .with("STEAM_0:0:173804217")
                                              ^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 44) spaces for indenting an expression spanning multiple lines.
                                              .and_return(76_561_198_307_874_162)
                                              ^^^^^^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 44) spaces for indenting an expression spanning multiple lines.
end

# A later continuation should not reuse an earlier column that was only valid
# because its receiver had a single-line block.
def household_size_options
  (0..10).map { |i| i }
         .unshift([t('common.prefer_not_to_answer'), -1])
         .unshift([nil, nil])
         ^^^^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 7) spaces for indenting an expression spanning multiple lines.
end

# Operator RHS: later continuation dots still align with the operator RHS base
def make_json_string expr
  Arel.quoted('"') \
  + expr
      .coalesce('')
      ^^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.coalesce` with `expr` on line 117.
      .replace('\\', '\\\\')
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.replace` with `expr` on line 117.
      .replace('"', '\"')
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.replace` with `expr` on line 117.
      .replace("\b", '\b')
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.replace` with `expr` on line 117.
      .replace("\f", '\f')
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.replace` with `expr` on line 117.
      .replace("\n", '\n')
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.replace` with `expr` on line 117.
      .replace("\r", '\r')
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.replace` with `expr` on line 117.
      .replace("\t", '\t') \
      ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.replace` with `expr` on line 117.
  + '"'
end

# Keyword condition: align to the full condition expression, not fallback indent
if stripe_mapping
    .select { |mapping| mapping.split('.')[0] == @model.name }
    ^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.select` with `stripe_mapping` on line 130.
    .size > 0
end

# Boolean keyword condition keeps the whole condition as the alignment base
if mixpanel_mapping && mixpanel_mapping
    .select { |mapping| mapping.split('.')[0] == @model.name }
    ^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.select` with `mixpanel_mapping && mixpanel_mapping` on line 136.
    .size > 0
end

# Nested matcher chain in a non-parenthesized argument falls back to outer indentation
it 'enqueues SetPointsCountryIdsJob for points without country_id' do
  expect { described_class.perform_now }.to \
    have_enqueued_job(DataMigrations::SetPointsCountryIdsJob)
      .with(point_without_country1.id)
      ^^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.
      .and have_enqueued_job(DataMigrations::SetPointsCountryIdsJob)
      ^^^^ Layout/MultilineMethodCallIndentation: Use 2 (not 4) spaces for indenting an expression spanning multiple lines.
      .with(point_without_country2.id)
end

# First continuation after a parenthesized receiver still aligns with the
# receiver's inline dot.
def prepare_cli_args(args, has_format_option)
  (args || '').split
            .yield_self { args_with_at_least_one_formatter(_1, has_format_option) }
            ^^^^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.yield_self` with `.split` on line 153.
            .yield_self { args_with_default_options(_1) }
end

# Descendant multiline block: align the matcher continuation with the receiver
it 'matches several changes' do
  expect { subject }.to change {
    value
  }.from(1).to(2)
        .and change {
        ^^^^ Layout/MultilineMethodCallIndentation: Align `.and` with `.to` on line 162.
          other
        }.from(3).to(4)
end

# When the first descendant block in source order is single-line, the
# multiline-block descendant deeper in the chain must NOT enable receiver-
# dot alignment. RuboCop's `each_descendant(:any_block).first` finds the
# earliest block (the single-line `.where { ... }`) and bails — so this
# `.with(:gpus, ...)` falls back to assignment-RHS alignment with `DB`,
# even though its argument contains a multiline `do ... end` block.
def candidate_hosts
  ds = DB[:vm_host]
    .with(:available_ipv4, DB[:ipv4_address]
    ^^^^^ Layout/MultilineMethodCallIndentation: Align `.with` with `DB[:vm_host]` on line 175.
      .where { row[:ip] =~ nil })
    .with(:gpus, DB[:pci_device]
    ^^^^^ Layout/MultilineMethodCallIndentation: Align `.with` with `DB[:vm_host]` on line 175.
      .select_append do
        gpu = build_object
        gpu.tap { |g| g.finalize }
      end)
end

# Hash pair value inside a lambda: the pair ancestor makes RuboCop align
# nested argument chains with the climbed left-hand side, not the inner
# argument's chain root.
virtual_attribute :normalized_state, :string, :arel => (lambda do |t|
  t.grouping(
    Arel::Nodes::Case.new
      .when(arel_table[:archived]).then(Arel.sql("'archived'"))
      ^^^^^ Layout/MultilineMethodCallIndentation: Align `.when` with `t.grouping(` on line 189.
      .when(arel_table[:orphaned]).then(Arel.sql("'orphaned'"))
      ^^^^^ Layout/MultilineMethodCallIndentation: Align `.when` with `t.grouping(` on line 189.
      .else(t.lower(
      ^^^^^ Layout/MultilineMethodCallIndentation: Align `.else` with `t.grouping(` on line 189.
              t.coalesce([t[:power_state], Arel.sql("'unknown'")])
      ))
  )
end)

# Assignment RHS followed by a multiline block receiver: later calls align
# with the assignment RHS, not the block-bearing continuation dot.
def gather_coverage_results
  result.to_h do |file_path, coverage_info|
    branch_by_line = coverage_info[:branches]
      .flat_map do |branch, data|
      ^^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.flat_map` with `coverage_info[:branches]` on line 203.
        data.map do |_then_or_else, _execution_count|
          { groupingLine: 1 }
        end
      end
      .group_by { |branch| branch[:groupingLine] }
      ^^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.group_by` with `coverage_info[:branches]` on line 203.
  end
end

# Hash pair value starting on the next line does not use the single-line block dot
foo(
  score_types:
    ReviewableScore
      .types
      ^^^^^^ Layout/MultilineMethodCallIndentation: Align `.types` with `ReviewableScore` on line 216.
      .filter { |k, v| k != :notify_user }
      ^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.filter` with `ReviewableScore` on line 216.
      .map { |k, v| { id: v, name: ReviewableScore.type_title(k) } },
      ^^^^ Layout/MultilineMethodCallIndentation: Align `.map` with `ReviewableScore` on line 216.
)

# `case` is not in RuboCop's UNALIGNED_RHS_TYPES, so a chain inside a `when`
# branch of an assigned `case` aligns with the `case` keyword expression.
def members(key, value)
  base_members = case key
  when :basket_size_id
    Member
      .joins(:current_or_future_membership)
      ^^^^^^ Layout/MultilineMethodCallIndentation: Align `.joins` with `case key` on line 225.
      .distinct
      ^^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.distinct` with `case key` on line 225.
  else
    Member.where(id: value)
  end
end

# Hash pair value whose chain base receiver is NOT a hash: every continuation
# line is measured against the left-hand side, with no block-chain escape.
def rows(bid)
  [
    {
      prev: @prev_rows
        .select { |r| r.budget_id == bid }
        ^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.select` with `@prev_rows` on line 240.
        .collect { |r| r.amount }
        ^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.collect` with `@prev_rows` on line 240.
        .inject(0) { |sum, x| sum + x }
        ^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.inject` with `@prev_rows` on line 240.
    }
  ]
end

# A block-bearing continuation after a multiline-block receiver falls through
# `find_continuation_node` to `first_call_alignment_node`, which walks past the
# block to the chain's first dotted call.
def actions
  RSpec::Sequencing.run("quit after a short time") do
    File.open(file_path, "wb") { |file| file.write("line1") }
  end
  .then("watch") do
  ^^^^^ Layout/MultilineMethodCallIndentation: Align `.then` with `.run` on line 252.
    reading.watch_this(watch_dir)
  end
  .then("wait") do
  ^^^^^ Layout/MultilineMethodCallIndentation: Align `.then` with `.run` on line 252.
    wait(2)
  end
end

# `each_descendant(:any_block).first` is parser pre-order: the outer `.map { }`
# block of the argument comes before the inner `.reject { ... }` one, and it is
# single-line, so no descendant-block alignment applies and the chain falls
# back to the assignment RHS.
def list
  subquery = joins(:dates, :translations)
    .select("events.*", "event_dates.start_at")
    ^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.select` with `joins(:dates, :translations)` on line 268.
    .select(Event::Translation.column_names
    ^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.select` with `joins(:dates, :translations)` on line 268.
                              .reject { |col|
              ["id", "event_id"].include?(col)
            }
                              .map { |col| "event_translations.#{col}" })
    .preload_all_dates
    ^^^^^^^^^^^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `.preload_all_dates` with `joins(:dates, :translations)` on line 268.
  subquery
end

# Safe navigation: `right_hand_side` is `dot.join(selector)`, so the reported
# source is `&.foo`, not `.foo`.
def links(item)
  item.edition_organisations
      .first
    &.organisation
    ^^^^^^^^^^^^^^ Layout/MultilineMethodCallIndentation: Align `&.organisation` with `.edition_organisations` on line 282.
end
