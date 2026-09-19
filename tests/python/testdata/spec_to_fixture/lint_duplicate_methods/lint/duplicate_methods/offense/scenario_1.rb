# nitrocop-filename: toplevel.rb
def some_method
  implement 1
end
def some_method
^^^^^^^^^^^^^^^ Lint/DuplicateMethods: Method `Object#some_method` is defined at both toplevel.rb:1 and toplevel.rb:4.
  implement 2
end
