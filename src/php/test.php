<?php
    ob_start();
    echo "Hi from PHP!";

    //flush();
    ob_flush();

    echo " Hi again!";