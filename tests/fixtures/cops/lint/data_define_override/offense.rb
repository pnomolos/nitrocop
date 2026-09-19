Bad = Data.define(:members)
                  ^^^^^^^^ Lint/DataDefineOverride: `:members` member overrides `Data#members` and it may be unexpected.

Bad = ::Data.define(:members)
                    ^^^^^^^^ Lint/DataDefineOverride: `:members` member overrides `Data#members` and it may be unexpected.

Bad = Data.define(:name, :members, :address)
                         ^^^^^^^^ Lint/DataDefineOverride: `:members` member overrides `Data#members` and it may be unexpected.

Bad = Data.define(:name, "members")
                         ^^^^^^^^^ Lint/DataDefineOverride: `"members"` member overrides `Data#members` and it may be unexpected.

Data.define(:members) do
            ^^^^^^^^ Lint/DataDefineOverride: `:members` member overrides `Data#members` and it may be unexpected.
  def members?
    !members.empty?
  end
end

Data.define(:members, :clone, :to_s)
                              ^^^^^ Lint/DataDefineOverride: `:to_s` member overrides `Data#to_s` and it may be unexpected.
                      ^^^^^^ Lint/DataDefineOverride: `:clone` member overrides `Data#clone` and it may be unexpected.
            ^^^^^^^^ Lint/DataDefineOverride: `:members` member overrides `Data#members` and it may be unexpected.
