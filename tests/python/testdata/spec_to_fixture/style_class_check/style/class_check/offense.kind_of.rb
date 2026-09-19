# nitrocop-config: EnforcedStyle: kind_of?
x.is_a? y
  ^^^^^ Style/ClassCheck: Prefer `Object#kind_of?` over `Object#is_a?`.

x&.is_a? y
   ^^^^^ Style/ClassCheck: Prefer `Object#kind_of?` over `Object#is_a?`.
