class RescueSpecs::C
  raise "message"
rescue => e
  ScratchPad << e.message
end

class Foo
  raise "bar"
rescue Baz => ex
end

# No superclass: not involved in inheritance, left to Lint/EmptyClass.
class MyClass
  self
end

class MyClass2
end
