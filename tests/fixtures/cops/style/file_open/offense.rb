File.open('file')
^^^^^^^^^^^^^^^^^ Style/FileOpen: `File.open` without a block may leak a file descriptor; use the block form.

::File.open('file')
^^^^^^^^^^^^^^^^^^^ Style/FileOpen: `File.open` without a block may leak a file descriptor; use the block form.

File.open('file').read
^^^^^^^^^^^^^^^^^ Style/FileOpen: `File.open` without a block may leak a file descriptor; use the block form.

File.open('file', 'w')
^^^^^^^^^^^^^^^^^^^^^^ Style/FileOpen: `File.open` without a block may leak a file descriptor; use the block form.

f = File.open('file')
    ^^^^^^^^^^^^^^^^^ Style/FileOpen: `File.open` without a block may leak a file descriptor; use the block form.

File.open("last")
^^^^^^^^^^^^^^^^^ Style/FileOpen: `File.open` without a block may leak a file descriptor; use the block form.
