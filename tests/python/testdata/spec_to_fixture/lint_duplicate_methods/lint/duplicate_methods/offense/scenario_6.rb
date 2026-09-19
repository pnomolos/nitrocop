# nitrocop-filename: test.rb
module A
  module B
    class C
      class << self
        def foo
        end

        def foo
        ^^^^^^^ Lint/DuplicateMethods: Method `A::B::C.foo` is defined at both test.rb:5 and test.rb:8.
        end
      end
    end
  end
end
