# Ported from vendor/rubocop @ v1.91.0
# spec/rubocop/cop/style/time_now_spec.rb.

Time.new
^^^^^^^^ Style/TimeNow: Prefer `Time.now` over `Time.new` to retrieve the current time.

Time.new()
^^^^^^^^^^ Style/TimeNow: Prefer `Time.now` over `Time.new` to retrieve the current time.

::Time.new
^^^^^^^^^^ Style/TimeNow: Prefer `Time.now` over `Time.new` to retrieve the current time.

Time&.new
^^^^^^^^^ Style/TimeNow: Prefer `Time.now` over `Time.new` to retrieve the current time.

Time.new.year
^^^^^^^^ Style/TimeNow: Prefer `Time.now` over `Time.new` to retrieve the current time.
