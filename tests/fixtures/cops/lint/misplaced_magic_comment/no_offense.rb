# frozen_string_literal: true
# encoding: ascii-8bit
case enc
when Encoding, false, nil
  # Encoding: force given encoding
  # false/nil: do not force encoding
end
# shareable_constant_value: literal
X = ['a']
