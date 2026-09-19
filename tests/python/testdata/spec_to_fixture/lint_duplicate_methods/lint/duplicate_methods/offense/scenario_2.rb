# nitrocop-filename: test.rb
class << A
  def some_method
    implement 1
  end
  def some_method
  ^^^^^^^^^^^^^^^ Lint/DuplicateMethods: Method `A.some_method` is defined at both test.rb:2 and test.rb:5.
    implement 2
  end
end
